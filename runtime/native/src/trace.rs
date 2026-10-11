//! What a program prints and the phases it times (`wrela.print` and `wrela.phase`, wrela_abi):
//! outputs it never reads back, from any of its threads (the `trace` effect, language.md §8). A
//! line is shown on stderr as it's made, unless the trace is quiet, and kept for whoever ran the
//! program. A phase runs on its thread until that thread's next one, or until the host ends it
//! ([`Trace::end_phase`], [`Trace::end_all`]); each phase's time is summed by name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::Instant;
use wasmtime::{Caller, Extern, Linker, SharedMemory};
use wrela_abi::{EXPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT};

/// What a program printed and timed: its lines, in order, and its phases, each in the order it
/// was first timed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traced {
    pub lines: Vec<String>,
    pub phases: Vec<Phase>,
}

/// A phase's name, its seconds summed, and how many times it was timed.
#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub name: String,
    pub seconds: f64,
    pub times: u32,
}

impl Traced {
    /// Whether it printed nothing and timed nothing.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.phases.is_empty()
    }

    /// The phases on one line: `survey 3.21 s, events 10.1 s (40×)`.
    pub fn phases_line(&self) -> String {
        let one = |p: &Phase| {
            let s = match p.seconds {
                10.0.. => format!("{:.1} s", p.seconds),
                0.01.. => format!("{:.2} s", p.seconds),
                0.001.. => format!("{:.2} ms", p.seconds * 1e3),
                _ => format!("{:.1} µs", p.seconds * 1e6),
            };
            if p.times > 1 {
                format!("{} {s} ({}×)", p.name, p.times)
            } else {
                format!("{} {s}", p.name)
            }
        };
        self.phases.iter().map(one).collect::<Vec<_>>().join(", ")
    }
}

/// A program's trace, shared by its threads' instances.
#[derive(Clone)]
pub struct Trace(Arc<Mutex<State>>);

struct State {
    /// What's shown before each line on stderr (`wrela: `, a constant's name); `None`: quiet.
    shown: Option<String>,
    traced: Traced,
    /// Each thread's phase: its index in `traced.phases`, and when it started.
    open: HashMap<ThreadId, (usize, Instant)>,
}

impl Trace {
    /// A trace that shows each line on stderr after `shown`, or (`None`) shows nothing.
    pub fn new(shown: Option<String>) -> Trace {
        Trace(Arc::new(Mutex::new(State {
            shown,
            traced: Traced::default(),
            open: HashMap::new(),
        })))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A line the program printed: shown now, unless quiet, and kept.
    pub fn print(&self, line: String) {
        let mut s = self.state();
        if let Some(shown) = &s.shown {
            eprintln!("{shown}{line}");
        }
        s.traced.lines.push(line);
    }

    /// A line the host says of the program, kept with its lines but not shown.
    pub fn keep(&self, line: String) {
        self.state().traced.lines.push(line);
    }

    /// Ends this thread's phase, and starts phase `name` on it (none, if `name` is empty).
    pub fn phase(&self, name: &str) {
        let now = Instant::now();
        let mut s = self.state();
        let me = std::thread::current().id();
        s.end(me, now);
        if name.is_empty() {
            return;
        }
        let i = match s.traced.phases.iter().position(|p| p.name == name) {
            Some(i) => i,
            None => {
                s.traced.phases.push(Phase { name: name.to_string(), seconds: 0.0, times: 0 });
                s.traced.phases.len() - 1
            }
        };
        s.traced.phases[i].times += 1;
        s.open.insert(me, (i, now));
    }

    /// Ends this thread's phase: the call the host made on it has ended.
    pub fn end_phase(&self) {
        let now = Instant::now();
        self.state().end(std::thread::current().id(), now);
    }

    /// Ends every thread's phase.
    pub fn end_all(&self) {
        let now = Instant::now();
        let mut s = self.state();
        let threads: Vec<ThreadId> = s.open.keys().copied().collect();
        for t in threads {
            s.end(t, now);
        }
    }

    /// The lines kept since the last call.
    pub fn take_lines(&self) -> Vec<String> {
        std::mem::take(&mut self.state().traced.lines)
    }

    /// The phases so far, those still open counted to now.
    pub fn phases(&self) -> Vec<Phase> {
        let now = Instant::now();
        let s = self.state();
        let mut phases = s.traced.phases.clone();
        for &(i, start) in s.open.values() {
            phases[i].seconds += (now - start).as_secs_f64();
        }
        phases
    }

    /// Everything traced, every phase ended.
    pub fn take(&self) -> Traced {
        self.end_all();
        std::mem::take(&mut self.state().traced)
    }

    /// Links `wrela.print` and `wrela.phase` to this trace. The program's lines and names are
    /// read from `memory`, or (`None`) from the memory the instance exports.
    pub fn link<T: 'static>(
        &self,
        linker: &mut Linker<T>,
        memory: Option<&SharedMemory>,
    ) -> wasmtime::Result<()> {
        let (t, m) = (self.clone(), memory.cloned());
        linker.func_wrap(
            IMPORT_MODULE,
            IMPORT_PRINT,
            move |c: Caller<'_, T>, at: u32, len: u32| {
                let line = text(c, m.as_ref(), at, len, "print")?;
                t.print(line);
                Ok(())
            },
        )?;
        let (t, m) = (self.clone(), memory.cloned());
        linker.func_wrap(
            IMPORT_MODULE,
            IMPORT_PHASE,
            move |c: Caller<'_, T>, at: u32, len: u32| {
                let name = text(c, m.as_ref(), at, len, "phase")?;
                t.phase(&name);
                Ok(())
            },
        )?;
        Ok(())
    }
}

impl State {
    fn end(&mut self, thread: ThreadId, now: Instant) {
        if let Some((i, start)) = self.open.remove(&thread) {
            self.traced.phases[i].seconds += (now - start).as_secs_f64();
        }
    }
}

/// The UTF-8 at `at..at + len` of the program's memory, for import `what`.
fn text<T>(
    mut caller: Caller<'_, T>,
    shared: Option<&SharedMemory>,
    at: u32,
    len: u32,
    what: &str,
) -> wasmtime::Result<String> {
    let past = || {
        wasmtime::format_err!("{what}({at}, {len}) reaches past the end of the program's memory")
    };
    let bytes = match shared {
        Some(m) => crate::shared::slice(m, at as usize, len as usize).ok_or_else(past)?.to_vec(),
        None => {
            let Some(Extern::Memory(m)) = caller.get_export(EXPORT_MEMORY) else {
                return Err(wasmtime::format_err!("{what}: the program exports no memory"));
            };
            let data = m.data(&caller);
            data.get(at as usize..at as usize + len as usize).ok_or_else(past)?.to_vec()
        }
    };
    String::from_utf8(bytes).map_err(|_| wasmtime::format_err!("{what}: the text isn't UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_runs_to_its_threads_next_and_is_summed_by_name() {
        let t = Trace::new(None);
        t.phase("a");
        t.phase("b");
        t.phase("a");
        t.end_phase();
        t.print("x".into());
        let traced = t.take();
        assert_eq!(traced.lines, vec!["x".to_string()]);
        let names: Vec<(&str, u32)> =
            traced.phases.iter().map(|p| (p.name.as_str(), p.times)).collect();
        assert_eq!(names, vec![("a", 2), ("b", 1)]);
        assert!(traced.phases.iter().all(|p| p.seconds >= 0.0));
    }

    #[test]
    fn each_thread_times_its_own_phase() {
        let t = Trace::new(None);
        t.phase("main");
        let other = t.clone();
        std::thread::spawn(move || other.phase("helper")).join().unwrap();
        // The helper's phase is still open; `take` ends it.
        let phases = t.take().phases;
        assert_eq!(
            phases.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["main", "helper"]
        );
    }
}
