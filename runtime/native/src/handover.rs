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
use crate::program::{Handing, Retired};
use crate::{Build, Error, Host, Result, compile_build};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::thread::JoinHandle;

pub(crate) type Preparation = JoinHandle<std::result::Result<(Build, Ready), String>>;

/// A build got ready, or being got ready, for a hand-over.
pub(crate) enum Prepared {
    /// Its thread, and the requests to answer when it ends.
    Going(Preparation, Vec<u32>),
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
fn prepare(dir: PathBuf, builder: Builder, have: std::collections::HashSet<u64>) -> Preparation {
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
                    match self.prepared.remove(&dir) {
                        Some(Prepared::Ready(_, keys)) => self.p().executor().let_go(&keys),
                        // Let the thread finish without blocking this call, but retain it so
                        // the host's GPU lock cannot be released before its work ends.
                        Some(Prepared::Going(job, waiting)) => {
                            self.cancelled_preparations.push(job);
                            let why = format!(
                                "the build in {} was let go before it was ready",
                                dir.display()
                            );
                            for r in waiting {
                                self.p().answer(r, Err(why.clone()));
                            }
                        }
                        Some(Prepared::Failed(_)) | None => {}
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
        self.cancelled_preparations.retain(|job| !job.is_finished());
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
        let old_init_done = old.init_done();
        let carried = old.retire().handed_over();
        let checkpoint = carried.checkpoint();
        let mut program = match self.start_carrying(&build, &dir, carried, false) {
            Ok(program) => program,
            Err((error, failed)) => {
                return self.resume_failed_hand_over(
                    &dir,
                    error,
                    checkpoint.restore(*failed).keeping(kept),
                    old_init_done,
                );
            }
        };
        if let Err(error) = program.run_init() {
            return self.resume_failed_hand_over(
                &dir,
                error,
                checkpoint.restore(program.retire()).keeping(kept),
                old_init_done,
            );
        }
        self.install_program(program);
        self.build = build;
        self.dir = dir;
        self.settle_handing()
    }

    fn resume_failed_hand_over(
        &mut self,
        dir: &Path,
        error: Error,
        mut carried: Retired<Gpu>,
        old_init_done: bool,
    ) -> Result<()> {
        carried.log(format!(
            "the hand-over to {} failed: {error}; {} starts again",
            dir.display(),
            self.dir.display()
        ));
        if let Err(restore) = self.restore_old(carried, old_init_done) {
            return Err(if self.program.is_some() {
                Error::Program(format!(
                    "the hand-over failed ({error}); the old build's init failed ({restore}); the host resumed with init deferred"
                ))
            } else {
                restore
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Io, Program};
    use crate::{Options, Timing};
    use std::sync::mpsc;
    use std::time::Duration;
    use wrela_abi::Manifest;
    use wrela_abi::stream::Encoder;

    #[test]
    #[ignore = "needs a GPU"]
    fn unprepared_work_finishes_before_the_host_releases_its_gpu() {
        let _turn = crate::lock::turn();
        let lock = crate::lock::GpuLock::acquire("preparation lifetime").unwrap();
        let batch = Encoder::new().unprepare("next").finish();
        let escaped: String = batch.iter().map(|b| format!("\\{b:02x}")).collect();
        let wasm = wat::parse_str(format!(
            r#"(module
                (import "wrela" "submit" (func $submit (param i32 i32)))
                (import "wrela" "memory" (memory 1 16384 shared))
                (export "memory" (memory 0))
                (data (i32.const 0) "{escaped}")
                (func (export "frame") (param f32 i32 i32)
                    (call $submit (i32.const 0) (i32.const {}))))"#,
            batch.len()
        ))
        .unwrap();
        let compiled = crate::program::compile_with(&wasm, None).unwrap();
        let manifest = Manifest::new("game.wasm");
        let gpu = Gpu::new(&manifest, &[], Timing::Off).unwrap();
        let mut program =
            Program::instantiate(&compiled, &manifest, gpu, Io::default(), false, 1).unwrap();
        program.hand_over_here();
        let mut host = Host {
            program: Some(program),
            build: Build { manifest, shaders: Vec::new(), wasm_hash: 0, compiled },
            dir: PathBuf::from("preparation-lifetime"),
            options: Options::default(),
            prepared: PreparedBuilds::new(),
            cancelled_preparations: Vec::new(),
            _lock: lock,
        };
        let builder = host.p().executor().builder();
        let (entered, release, finished) = (mpsc::channel(), mpsc::channel(), mpsc::channel());
        let job = std::thread::spawn(move || {
            entered.0.send(()).unwrap();
            release.1.recv().unwrap();
            drop(builder);
            finished.0.send(()).unwrap();
            Err("the test discarded this preparation".to_string())
        });
        host.prepared.insert(host.dir.join("next"), Prepared::Going(job, Vec::new()));
        entered.1.recv_timeout(Duration::from_secs(5)).unwrap();
        let dropping = mpsc::channel();
        let returned = mpsc::channel();
        let controller = std::thread::spawn(move || {
            let frame_returned = dropping.1.recv_timeout(Duration::from_secs(5)).is_ok();
            let returned_early =
                frame_returned && returned.1.recv_timeout(Duration::from_millis(200)).is_ok();
            release.0.send(()).unwrap();
            (frame_returned, returned_early)
        });
        host.frame(0.0, 4, 4).expect("unprepare returns while its worker is still blocked");
        let _ = dropping.0.send(());
        drop(host);
        let _ = returned.0.send(());
        let (frame_returned, returned_early) = controller.join().unwrap();
        finished.1.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(frame_returned, "unprepare blocked its frame until preparation finished");
        assert!(
            !returned_early,
            "the host released its GPU while preparation still held the device"
        );
    }
}
