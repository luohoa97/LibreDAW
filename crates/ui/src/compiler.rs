// SPDX-License-Identifier: GPL-3.0-or-later
//! The compiler thread (SPEC 4.2).
//!
//! The GTK thread sends a `CompileJob` for a document revision. The thread
//! keeps only the latest pending job (coalescing), compiles it off the GTK
//! thread, and leaves the result in a one-slot mailbox. A newer result
//! replaces an older one that the GTK thread has not taken yet. The newest
//! revision is never discarded: if the engine's state ring is full, the GTK
//! thread puts the result back and retries on its next tick.
//!
//! The thread is generic over the compiled type so the logic is testable
//! without the engine; `engine_adapter` supplies the real compile function.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use protocol::model::Project;

use crate::slots::SlotAllocator;

/// Everything the compile function needs, captured on the GTK thread.
#[derive(Clone)]
pub struct CompileJob {
    pub revision: u64,
    pub project: Arc<Project>,
    pub slots: SlotAllocator,
    pub sample_rate: f64,
}

struct State<T> {
    pending: Option<CompileJob>,
    ready: Option<(u64, T)>,
    compiling: bool,
    stop: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
    /// Jobs received, and compiles run. For tests and diagnostics.
    received: AtomicU64,
    compiled: AtomicU64,
}

pub struct Compiler<T: Send + 'static> {
    shared: Arc<Shared<T>>,
    handle: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Compiler<T> {
    pub fn spawn(compile: impl Fn(&CompileJob) -> T + Send + 'static) -> Compiler<T> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: None,
                ready: None,
                compiling: false,
                stop: false,
            }),
            wake: Condvar::new(),
            received: AtomicU64::new(0),
            compiled: AtomicU64::new(0),
        });
        let s = shared.clone();
        let handle = std::thread::Builder::new()
            .name("compiler".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut st = s.state.lock().expect("compiler lock");
                        loop {
                            if st.stop {
                                return;
                            }
                            if let Some(j) = st.pending.take() {
                                st.compiling = true;
                                break j;
                            }
                            st = s.wake.wait(st).expect("compiler wait");
                        }
                    };
                    let out = compile(&job);
                    s.compiled.fetch_add(1, Ordering::Relaxed);
                    let mut st = s.state.lock().expect("compiler lock");
                    st.compiling = false;
                    // Newest wins; an older unclaimed result is dropped.
                    let newer = st.ready.as_ref().is_none_or(|(r, _)| *r <= job.revision);
                    if newer {
                        st.ready = Some((job.revision, out));
                    }
                }
            })
            .expect("spawn compiler thread");
        Compiler {
            shared,
            handle: Some(handle),
        }
    }

    /// Queues a job, replacing any job that has not started yet.
    pub fn request(&self, job: CompileJob) {
        self.shared.received.fetch_add(1, Ordering::Relaxed);
        let mut st = self.shared.state.lock().expect("compiler lock");
        st.pending = Some(job);
        self.shared.wake.notify_one();
    }

    /// Takes the newest finished result, if any.
    pub fn take_ready(&self) -> Option<(u64, T)> {
        self.shared
            .state
            .lock()
            .expect("compiler lock")
            .ready
            .take()
    }

    /// Returns a result the engine did not accept (state ring full). If a
    /// newer result arrived meanwhile, the returned one is dropped.
    pub fn put_back(&self, revision: u64, value: T) {
        let mut st = self.shared.state.lock().expect("compiler lock");
        if st.ready.is_none() {
            st.ready = Some((revision, value));
        }
    }

    /// True when nothing is pending or running and no result waits.
    pub fn is_idle(&self) -> bool {
        let st = self.shared.state.lock().expect("compiler lock");
        st.pending.is_none() && !st.compiling
    }

    pub fn jobs_received(&self) -> u64 {
        self.shared.received.load(Ordering::Relaxed)
    }

    pub fn jobs_compiled(&self) -> u64 {
        self.shared.compiled.load(Ordering::Relaxed)
    }

    /// Waits until idle (tests).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if self.is_idle() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }
}

impl<T: Send + 'static> Drop for Compiler<T> {
    fn drop(&mut self) {
        {
            let mut st = self.shared.state.lock().expect("compiler lock");
            st.stop = true;
            self.shared.wake.notify_one();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn job(rev: u64) -> CompileJob {
        CompileJob {
            revision: rev,
            project: Arc::new(Project::empty()),
            slots: SlotAllocator::new(),
            sample_rate: 48000.0,
        }
    }

    #[test]
    fn compiles_and_hands_back_the_result() {
        let c = Compiler::spawn(|j| j.revision * 10);
        c.request(job(3));
        assert!(c.wait_idle(Duration::from_secs(5)));
        assert_eq!(c.take_ready(), Some((3, 30)));
        assert_eq!(c.take_ready(), None);
    }

    #[test]
    fn coalesces_to_the_latest_pending_job_and_never_drops_it() {
        // Block the first compile so later requests pile up.
        let (gate_tx, gate_rx) = channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let (started_tx, started_rx) = channel::<u64>();
        let started_tx = Mutex::new(started_tx);
        let c = Compiler::spawn(move |j| {
            started_tx.lock().unwrap().send(j.revision).unwrap();
            if j.revision == 1 {
                gate_rx.lock().unwrap().recv().unwrap();
            }
            j.revision
        });
        c.request(job(1));
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        for r in 2..=50 {
            c.request(job(r));
        }
        gate_tx.send(()).unwrap();
        assert!(c.wait_idle(Duration::from_secs(5)));
        // Revision 1 ran, then only the latest of the pile (50).
        let mut ran = vec![];
        while let Ok(r) = started_rx.try_recv() {
            ran.push(r);
        }
        assert_eq!(ran, vec![50]);
        assert_eq!(c.jobs_received(), 50);
        assert_eq!(c.jobs_compiled(), 2);
        assert_eq!(c.take_ready(), Some((50, 50)), "latest is kept");
    }

    #[test]
    fn newer_result_replaces_an_unclaimed_older_one() {
        let c = Compiler::spawn(|j| j.revision);
        c.request(job(1));
        assert!(c.wait_idle(Duration::from_secs(5)));
        c.request(job(2));
        assert!(c.wait_idle(Duration::from_secs(5)));
        assert_eq!(c.take_ready(), Some((2, 2)));
    }

    #[test]
    fn put_back_keeps_a_rejected_result_unless_a_newer_one_arrived() {
        let c = Compiler::spawn(|j| j.revision);
        c.request(job(1));
        assert!(c.wait_idle(Duration::from_secs(5)));
        let (r, v) = c.take_ready().unwrap();
        c.put_back(r, v);
        assert_eq!(c.take_ready(), Some((1, 1)));
        // Engine full: result taken, newer one compiled, old one put back.
        c.request(job(5));
        assert!(c.wait_idle(Duration::from_secs(5)));
        let (r, v) = c.take_ready().unwrap();
        c.request(job(6));
        assert!(c.wait_idle(Duration::from_secs(5)));
        c.put_back(r, v);
        assert_eq!(c.take_ready(), Some((6, 6)));
    }

    #[test]
    fn drop_joins_the_thread() {
        let c = Compiler::spawn(|j| j.revision);
        c.request(job(1));
        drop(c);
    }
}
