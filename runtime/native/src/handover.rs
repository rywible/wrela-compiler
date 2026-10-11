//! One build hands its place to another in the same process, on the same device
//! (`std::reload::prepare`, `unprepare` and `hand_over`): a game made of several builds, such as
//! a tower of floors, each started when the player reaches it.
//!
//! - **Prepare:** a thread of its own compiles the build's WASM and builds the pipelines the
//!   device doesn't hold yet, while the program runs. The pipelines wait as spares
//!   ([`Gpu::keep_ready`]) until a swap takes them or the program lets the build go.
//! - **Hand over:** after the call that asked, the build is got ready (at once, if it isn't),
//!   then the running program stops and the build starts with `init`, given what the old one
//!   kept. A build that can't be got ready fails the request, and the old program goes on. If
//!   the new one's `init` fails, the old build starts again in its place with the bytes it had
//!   kept: the screen never goes black.

use crate::gpu::{Builder, Gpu, Ready, pipeline_key};
use crate::program::{Handing, Program};
use crate::{Build, Host, Result, compile_build, workers};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::thread::JoinHandle;

/// A build got ready, or being got ready, for a hand-over.
pub(crate) enum Prepared {
    /// Its thread, and the requests to answer when it ends.
    Going(JoinHandle<std::result::Result<(Build, Ready), String>>, Vec<u32>),
    /// Ready: the build, and the keys of all its pipelines.
    Ready(Build, Vec<u64>),
    Failed(String),
}

/// The builds a host has got ready, by directory.
pub(crate) type PreparedBuilds = HashMap<PathBuf, Prepared>;

/// The directory `path` names from `dir`: `..` parts take a part off.
fn beside(dir: &Path, path: &str) -> PathBuf {
    let mut out = dir.to_path_buf();
    for part in Path::new(path).components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(p) => out.push(p),
            _ => {}
        }
    }
    out
}

/// Compiles the build in `dir` and builds the pipelines `builder`'s device doesn't hold
/// (`have`): on a thread of its own.
fn prepare(
    dir: PathBuf,
    builder: Builder,
    have: std::collections::HashSet<u64>,
) -> JoinHandle<std::result::Result<(Build, Ready), String>> {
    std::thread::spawn(move || {
        let build = compile_build(&dir, None).map_err(|e| e.to_string())?;
        let ready =
            builder.build_all(&build.manifest, &build.shaders, &have).map_err(|e| e.to_string())?;
        Ok((build, ready))
    })
}

fn keys_of(build: &Build) -> Vec<u64> {
    build.manifest.pipelines.iter().zip(&build.shaders).map(|(p, s)| pipeline_key(p, s)).collect()
}

impl Host {
    /// Carries out what the program asked of other builds in the call that just returned, and
    /// answers what's ready: after each frame.
    pub(crate) fn settle_handing(&mut self) -> Result<()> {
        let mut hand = None;
        for h in self.p().take_handing() {
            match h {
                Handing::Prepare { request, path } => {
                    let dir = beside(&self.dir, &path);
                    self.start_preparing(&dir);
                    let now = match self.prepared.get_mut(&dir) {
                        Some(Prepared::Going(_, waiting)) => {
                            waiting.push(request);
                            None
                        }
                        Some(Prepared::Ready(..)) => Some(Ok(Vec::new())),
                        Some(Prepared::Failed(why)) => Some(Err(why.clone())),
                        None => unreachable!("start_preparing made it"),
                    };
                    if let Some(answer) = now {
                        self.p().answer(request, answer);
                    }
                }
                Handing::Unprepare { path } => {
                    let dir = beside(&self.dir, &path);
                    // A thread still going is left to end; what it built is dropped with it.
                    if let Some(Prepared::Ready(_, keys)) = self.prepared.remove(&dir) {
                        self.p().executor().let_go(&keys);
                    }
                }
                Handing::HandOver { request, path } => {
                    // The last one asked is the one carried out.
                    if let Some((earlier, _)) = hand.replace((request, path)) {
                        self.p().answer(earlier, Err("a later hand-over took its place".into()));
                    }
                }
            }
        }
        self.poll_prepared(None);
        if let Some((request, path)) = hand {
            let dir = beside(&self.dir, &path);
            self.hand_over(request, dir)?;
        }
        Ok(())
    }

    /// Starts getting the build in `dir` ready, unless it is or it's on its way.
    fn start_preparing(&mut self, dir: &Path) {
        if self.prepared.contains_key(dir) {
            return;
        }
        let gpu: &mut Gpu = self.p().executor();
        let job = prepare(dir.to_path_buf(), gpu.builder(), gpu.pipeline_keys());
        self.prepared.insert(dir.to_path_buf(), Prepared::Going(job, Vec::new()));
    }

    /// Takes what each preparing thread that has ended made, and answers its requests; with
    /// `wait`, waits for that build's thread first.
    fn poll_prepared(&mut self, wait: Option<&Path>) {
        let ended: Vec<PathBuf> = self
            .prepared
            .iter()
            .filter(|(d, p)| {
                matches!(p, Prepared::Going(job, _) if job.is_finished() || Some(d.as_path()) == wait)
            })
            .map(|(d, _)| d.clone())
            .collect();
        for dir in ended {
            let Some(Prepared::Going(job, waiting)) = self.prepared.remove(&dir) else {
                continue;
            };
            let outcome = job.join().unwrap_or_else(|_| Err("getting it ready panicked".into()));
            let answer = match outcome {
                Ok((build, ready)) => {
                    let keys = keys_of(&build);
                    self.p().executor().keep_ready(ready, &keys);
                    self.prepared.insert(dir, Prepared::Ready(build, keys));
                    Ok(Vec::new())
                }
                Err(why) => {
                    let why = format!("the build in {} isn't ready: {why}", dir.display());
                    self.prepared.insert(dir, Prepared::Failed(why.clone()));
                    Err(why)
                }
            };
            for r in waiting {
                self.p().answer(r, answer.clone());
            }
        }
    }

    /// Hands the program's place to the build in `dir` (`std::reload::hand_over`'s request
    /// `request`).
    fn hand_over(&mut self, request: u32, dir: PathBuf) -> Result<()> {
        self.start_preparing(&dir);
        self.poll_prepared(Some(&dir));
        let build = match self.prepared.remove(&dir) {
            Some(Prepared::Ready(build, _)) => build,
            Some(Prepared::Failed(why)) => {
                self.p().answer(request, Err(why));
                return Ok(());
            }
            _ => unreachable!("poll_prepared waited for it"),
        };
        let kept = self.p().keeping().to_vec();
        let old = self.program.take().expect("the host has a program");
        let mut carried = old.retire().handed_over();
        program_reload(&mut carried.executor, &build)?;
        let mut program = Program::instantiate_carrying(
            &build.compiled,
            &build.manifest,
            carried,
            self.options.io(&dir),
            workers(self.options.workers),
            false,
        )?;
        program.hand_over_here();
        if let Err(e) = program.run_init() {
            // Its `init` failed: the build that handed over starts again, with what it kept.
            let mut carried = program.retire().keeping(kept);
            carried.log(format!(
                "the hand-over to {} failed: {e}; {} starts again",
                dir.display(),
                self.dir.display()
            ));
            let again = compile_build(&self.dir, None)?;
            program_reload(&mut carried.executor, &again)?;
            let mut program = Program::instantiate_carrying(
                &again.compiled,
                &again.manifest,
                carried,
                self.options.io(&self.dir),
                workers(self.options.workers),
                true,
            )?;
            program.hand_over_here();
            self.program = Some(program);
            return Ok(());
        }
        self.program = Some(program);
        self.wasm_hash = build.wasm_hash;
        self.dir = dir;
        Ok(())
    }
}

/// The executor's resources forgotten and `build`'s pipelines taken (from the spares where it
/// can).
fn program_reload(gpu: &mut Gpu, build: &Build) -> Result<()> {
    crate::program::Executor::reload(gpu, &build.manifest, &build.shaders)
}
