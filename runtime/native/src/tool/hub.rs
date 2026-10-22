//! A run's shared state (language.md §15.5): the requests the tool's threads made and the answers
//! that came, the timers, a tool test's fake clock, and what each of the tool's threads is doing,
//! behind one lock, with a condition variable that every answer signals.
//!
//! A thread of the tool's that waits (`tool_wait`, `tool_any`, a tool task's `tool_join`) blocks
//! here, on the condition variable. It wakes on every change and at least every [`WAKE`]: then
//! it fires the timers that are due, moves a fake clock on if every thread waits on the clock
//! alone, and looks for its answer and for the run's stop reasons. So a stop (`--timeout`, Ctrl-C,
//! a helper's trap, stuck work) ends a wait within about 10 ms.
//!
//! **The fake clock with threads** (a tool test's, §13.2). Each of the tool's threads is
//! [`Thread::Running`], [`Thread::Waiting`] on a set of requests, [`Thread::Joining`] a task's
//! slot, or [`Thread::Idle`] (a helper that waits for work and holds none). Under the lock, the
//! clock moves to the soonest timer only when every outstanding request is the clock's (a timer,
//! a fake site's held answer) or one that nothing in a tool test answers (a served post); every
//! thread is idle, waits on requests none of which is answered, or joins a slot whose task still
//! runs; and no slot is queued while a helper is idle (that helper is about to take it). A thread
//! that runs `std::par`'s work counts as running, which is conservative.

use super::services::Answer;
use super::{End, Reasons};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wasmtime::SharedMemory;
use wrela_abi::memory::{
    SLOT_QUEUED, SLOT_STARTED, SLOT_STATE, TASK_SLOT_COUNT, TASK_SLOT_SIZE, TASK_SLOTS,
};

/// How often a blocked thread wakes, at least, to look for the run's stop reasons.
pub(crate) const WAKE: Duration = Duration::from_millis(10);

/// What one of the tool's threads is doing, as the fake clock reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Thread {
    /// Running the tool's code (also `std::par`'s waits, which the host doesn't see).
    Running,
    /// In `tool_wait` or `tool_any`: runnable once one of these is answered.
    Waiting(Vec<u32>),
    /// In `tool_join`: runnable once the task in this slot is no longer running.
    Joining(u32),
    /// A helper waiting for work, holding none.
    Idle,
}

/// A tool test's fake clock: the time since the run started, which moves only when the tool
/// waits on nothing but the clock, and the date at the start.
pub(crate) struct Fake {
    pub now: Duration,
    pub utc: Duration,
}

/// The state behind the lock.
pub(crate) struct State {
    /// The number the next request gets.
    next: u32,
    /// Requests made and not answered yet.
    outstanding: HashSet<u32>,
    /// Answers the tool may take, each with the order it came in.
    answered: HashMap<u32, (u64, Answer)>,
    arrivals: u64,
    /// Answers given at a time on the run's clock, soonest first: timers', and a fake site's
    /// held for its delay.
    timed: BinaryHeap<Reverse<(Duration, u32)>>,
    timed_answers: HashMap<u32, Answer>,
    /// When requests a worker carries out may be answered, if not at once: a fake site's delay.
    due: HashMap<u32, Duration>,
    /// Requests a service answers itself when something happens (`Work::Later`), not answered
    /// yet: in a tool test, nothing makes that happen, so the fake clock moves while they wait.
    later: HashSet<u32>,
    pub(crate) fake: Option<Fake>,
    origin: Instant,
    /// Each of the tool's threads by its number, while it runs.
    threads: HashMap<u32, Thread>,
}

impl State {
    /// The time on the run's clock: since its start, real or fake.
    pub(crate) fn now(&self) -> Duration {
        match &self.fake {
            Some(f) => f.now,
            None => self.origin.elapsed(),
        }
    }

    /// The date now, as `time/utc` answers it: the system's, or the fake clock's.
    pub(crate) fn utc(&self) -> Answer {
        let now = match &self.fake {
            Some(f) => f.utc + f.now,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default(),
        };
        let mut payload = vec![0];
        payload.extend_from_slice(&now.as_secs().to_le_bytes());
        payload.extend_from_slice(&now.subsec_nanos().to_le_bytes());
        Answer::Bytes(payload)
    }

    /// `answer` to `request`, given to the tool now, or at the time it's due.
    fn answered(&mut self, request: u32, answer: Answer) {
        if !self.outstanding.contains(&request) {
            return;
        }
        match self.due.remove(&request) {
            Some(at) if at > self.now() => {
                self.timed.push(Reverse((at, request)));
                self.timed_answers.insert(request, answer);
            }
            _ => self.arrive(request, answer),
        }
    }

    fn arrive(&mut self, request: u32, answer: Answer) {
        self.outstanding.remove(&request);
        self.later.remove(&request);
        self.arrivals += 1;
        self.answered.insert(request, (self.arrivals, answer));
    }

    /// Request `request` answers, with no bytes, once `after` has passed on the run's clock.
    pub(crate) fn timer(&mut self, request: u32, after: Duration) {
        let at = self.now() + after;
        self.timed.push(Reverse((at, request)));
        self.timed_answers.insert(request, Answer::Bytes(vec![0]));
    }

    /// Request `request`'s answer, from a worker, is given no sooner than `after` from now.
    pub(crate) fn hold(&mut self, request: u32, after: Duration) {
        let at = self.now() + after;
        self.due.insert(request, at);
    }

    /// Request `request` waits for an event a service answers it on.
    pub(crate) fn wait_later(&mut self, request: u32) {
        self.later.insert(request);
    }

    /// Gives the timers that are due their answers; on a fake clock, first moves the clock to
    /// the soonest if every thread waits on the clock alone. Whether any fired.
    fn fire(&mut self, memory: &SharedMemory) -> bool {
        if self.fake.is_some()
            && let Some(&Reverse((at, _))) = self.timed.peek()
            && self.quiet(memory)
            && let Some(f) = &mut self.fake
        {
            f.now = f.now.max(at);
        }
        let now = self.now();
        let mut fired = false;
        while let Some(&Reverse((at, request))) = self.timed.peek()
            && at <= now
        {
            self.timed.pop();
            let answer = self.timed_answers.remove(&request).unwrap_or(Answer::Failed);
            self.arrive(request, answer);
            fired = true;
        }
        fired
    }

    /// Whether nothing but the clock can wake the tool: the fake clock's rule (the module's
    /// docs).
    fn quiet(&self, memory: &SharedMemory) -> bool {
        if self.outstanding.len() != self.timed_answers.len() + self.later.len() {
            return false;
        }
        let state = |slot: u32| crate::shared::load_u32(memory, slot + SLOT_STATE);
        let waiting = self.threads.values().all(|t| match t {
            Thread::Running => false,
            Thread::Idle => true,
            Thread::Waiting(set) => !set.iter().any(|r| self.answered.contains_key(r)),
            Thread::Joining(slot) => state(*slot) == SLOT_STARTED,
        });
        if !waiting {
            return false;
        }
        let idle = self.threads.values().any(|t| *t == Thread::Idle);
        !(idle
            && (0..TASK_SLOT_COUNT).any(|s| state(TASK_SLOTS + s * TASK_SLOT_SIZE) == SLOT_QUEUED))
    }

    /// The soonest timer's time on the real clock, from now: how long a blocked thread may
    /// sleep before it fires.
    fn next_timer(&self) -> Option<Duration> {
        if self.fake.is_some() {
            return None;
        }
        let Reverse((at, _)) = self.timed.peek()?;
        Some(at.saturating_sub(self.now()))
    }
}

/// The run's shared state and its condition variable.
pub(crate) struct Hub {
    state: Mutex<State>,
    wake: Condvar,
}

impl Hub {
    /// A run that started now, on a fake clock from this date if it's a tool test's.
    pub(crate) fn new(fake: Option<(u64, u32)>) -> Hub {
        Hub {
            state: Mutex::new(State {
                next: 1,
                outstanding: HashSet::new(),
                answered: HashMap::new(),
                arrivals: 0,
                timed: BinaryHeap::new(),
                timed_answers: HashMap::new(),
                due: HashMap::new(),
                later: HashSet::new(),
                fake: fake.map(|(s, n)| Fake { now: Duration::ZERO, utc: Duration::new(s, n) }),
                origin: Instant::now(),
                threads: HashMap::new(),
            }),
            wake: Condvar::new(),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        crate::locked(&self.state)
    }

    /// Wakes every blocked thread to look again.
    pub(crate) fn notify(&self) {
        self.wake.notify_all();
    }

    /// A new request's number, outstanding from now on.
    pub(crate) fn make(&self) -> u32 {
        let mut s = self.lock();
        let request = s.next;
        s.next = s.next.wrapping_add(1).max(1);
        s.outstanding.insert(request);
        request
    }

    /// Gives `answer` to `request` (from a worker, a service or the host itself), and wakes the
    /// threads that wait.
    pub(crate) fn deliver(&self, request: u32, answer: Answer) {
        self.lock().answered(request, answer);
        self.notify();
    }

    /// Takes request `request`'s answer, which the tool reads: `None` if it isn't answered.
    /// The threads that wait look again: one that waits on it waits on nothing now.
    pub(crate) fn take(&self, request: u32) -> Option<Answer> {
        let taken = self.lock().answered.remove(&request).map(|(_, a)| a);
        self.notify();
        taken
    }

    /// Gives back an answer `take` took, which the tool couldn't read.
    pub(crate) fn untake(&self, request: u32, answer: Answer) {
        let mut s = self.lock();
        s.arrivals += 1;
        let n = s.arrivals;
        s.answered.insert(request, (n, answer));
    }

    /// Thread `thread` is now in `state`; the threads that wait look again.
    pub(crate) fn set(&self, thread: u32, state: Thread) {
        self.lock().threads.insert(thread, state);
        self.notify();
    }

    /// Thread `thread` has returned: it runs nothing more.
    pub(crate) fn gone(&self, thread: u32) {
        self.lock().threads.remove(&thread);
        self.notify();
    }

    /// Blocks thread `thread`, in `state`, until `ready` gives something, or the run must stop
    /// (`reasons`, or their latch once the tool's own thread has returned): then the reason. A
    /// stop wins over an answer that is ready, and a wait on requests is checked again on every
    /// wake: one whose answer another thread took ends the wait as a wait on nothing.
    pub(crate) fn block<R>(
        &self,
        thread: u32,
        state: Thread,
        memory: &SharedMemory,
        reasons: &Reasons,
        mut ready: impl FnMut(&State) -> Option<R>,
    ) -> Result<R, End> {
        let waits_on = match &state {
            Thread::Waiting(set) => set.clone(),
            _ => Vec::new(),
        };
        let mut s = self.lock();
        s.threads.insert(thread, state);
        // Its state changed: another thread may now find the run quiet.
        self.notify();
        let out = loop {
            if let Some(why) = reasons.now() {
                reasons.latch();
                break Err(why);
            }
            if reasons.latched() {
                break Err(End::Interrupted);
            }
            let fired = s.fire(memory);
            if fired {
                self.notify();
            }
            // The whole set, before any answer in it is accepted: a request taken by another
            // thread ends the wait, though another of the set is answered.
            if !waits_on.iter().all(|&r| Hub::known(&s, r)) {
                break Err(End::WaitsOnNothing);
            }
            if let Some(r) = ready(&s) {
                break Ok(r);
            }
            if fired {
                // A fake clock may move on again at once: no wait between its timers.
                continue;
            }
            let mut wait = WAKE;
            if let Some(t) = s.next_timer() {
                wait = wait.min(t);
            }
            if let Some(d) = reasons.deadline {
                wait = wait.min(d.saturating_duration_since(Instant::now()));
            }
            s = match self.wake.wait_timeout(s, wait) {
                Ok((s, _)) => s,
                Err(poisoned) => poisoned.into_inner().0,
            };
        };
        s.threads.insert(thread, Thread::Running);
        drop(s);
        self.notify();
        out
    }

    /// Whether `request` is one a thread may wait on: outstanding, or answered and not taken.
    pub(crate) fn known(s: &State, request: u32) -> bool {
        s.outstanding.contains(&request) || s.answered.contains_key(&request)
    }

    /// The answer to `request`, if it has come: its length, or `REQUEST_FAILED`.
    pub(crate) fn status(s: &State, request: u32) -> Option<i32> {
        match s.answered.get(&request)? {
            (_, Answer::Failed) => Some(wrela_abi::REQUEST_FAILED),
            (_, Answer::Bytes(b)) => Some(b.len() as i32),
        }
    }

    /// The index in `requests` of the one answered first, if any is.
    pub(crate) fn first(s: &State, requests: &[u32]) -> Option<u32> {
        let mut best: Option<(u64, u32)> = None;
        for (i, r) in requests.iter().enumerate() {
            if let Some((n, _)) = s.answered.get(r)
                && best.is_none_or(|(m, _)| *n < m)
            {
                best = Some((*n, i as u32));
            }
        }
        best.map(|(_, i)| i)
    }

    /// Whether the task in `slot` no longer runs on another thread.
    pub(crate) fn joined(memory: &SharedMemory, slot: u32) -> bool {
        let state = crate::shared::load_u32(memory, slot + SLOT_STATE);
        state != SLOT_STARTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// A memory for the hub's checks, which a run on the real clock never reads.
    fn memory() -> SharedMemory {
        let mut config = wasmtime::Config::new();
        config.wasm_threads(true).shared_memory(true);
        let engine = wasmtime::Engine::new(&config).expect("an engine");
        SharedMemory::new(&engine, wasmtime::MemoryType::shared(1, 1)).expect("a memory")
    }

    fn reasons(interrupt: bool, deadline: Option<Instant>) -> Reasons {
        Reasons {
            interrupt: Some(Arc::new(AtomicBool::new(interrupt))),
            deadline,
            stuck: Arc::new(crate::tool::services::Stuck::new(Duration::from_secs(5))),
            trapped: Arc::new(std::sync::OnceLock::new()),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn a_stop_wins_over_an_answer_that_is_ready() {
        let hub = Hub::new(None);
        let r = hub.make();
        hub.deliver(r, Answer::Bytes(vec![0]));
        let got = hub.block(0, Thread::Waiting(vec![r]), &memory(), &reasons(true, None), |s| {
            Hub::status(s, r)
        });
        assert_eq!(got, Err(End::Interrupted));
        // Without the stop, the answer is ready.
        let got = hub.block(0, Thread::Waiting(vec![r]), &memory(), &reasons(false, None), |s| {
            Hub::status(s, r)
        });
        assert_eq!(got, Ok(1));
    }

    #[test]
    fn a_wait_whose_answer_another_thread_took_waits_on_nothing() {
        let hub = Arc::new(Hub::new(None));
        let r = hub.make();
        let (h, memory) = (hub.clone(), memory());
        // Bounded, so a wait that never rechecks ends at the deadline instead.
        let deadline = Instant::now() + Duration::from_secs(3);
        let waiter = std::thread::spawn(move || {
            h.block(3, Thread::Waiting(vec![r]), &memory, &reasons(false, Some(deadline)), |s| {
                Hub::status(s, r)
            })
        });
        // Once it waits, the answer comes and another thread takes it, in one step: the waiter
        // never sees it ready.
        while hub.lock().threads.get(&3) != Some(&Thread::Waiting(vec![r])) {
            std::thread::sleep(Duration::from_millis(1));
        }
        {
            let mut s = hub.lock();
            s.answered(r, Answer::Bytes(vec![0]));
            s.answered.remove(&r);
        }
        hub.notify();
        assert_eq!(waiter.join().expect("the waiter"), Err(End::WaitsOnNothing));
    }

    #[test]
    fn any_whose_request_another_thread_took_waits_on_nothing_though_another_is_answered() {
        let hub = Arc::new(Hub::new(None));
        let (a, b) = (hub.make(), hub.make());
        let (h, memory) = (hub.clone(), memory());
        let deadline = Instant::now() + Duration::from_secs(3);
        let waiter = std::thread::spawn(move || {
            let set = vec![a, b];
            h.block(
                3,
                Thread::Waiting(set.clone()),
                &memory,
                &reasons(false, Some(deadline)),
                |s| Hub::first(s, &set),
            )
        });
        while hub.lock().threads.get(&3) != Some(&Thread::Waiting(vec![a, b])) {
            std::thread::sleep(Duration::from_millis(1));
        }
        // In one step: `a` answered and taken by another thread, and `b` answered.
        {
            let mut s = hub.lock();
            s.answered(a, Answer::Bytes(vec![0]));
            s.answered.remove(&a);
            s.answered(b, Answer::Bytes(vec![0]));
        }
        hub.notify();
        assert_eq!(waiter.join().expect("the waiter"), Err(End::WaitsOnNothing));
    }
}
