// SPDX-License-Identifier: GPL-3.0-or-later
//! Work that must not block the GTK thread (SPEC 3.1): save, autosave,
//! export. `spawn` runs a closure on a worker thread; `poll`, called from
//! the 10 ms source, hands the result to a callback on the GTK thread.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

type Poller = Box<dyn FnMut() -> bool>;

#[derive(Default)]
pub struct Tasks {
    pollers: RefCell<Vec<Poller>>,
}

impl Tasks {
    pub fn new() -> Tasks {
        Tasks::default()
    }

    /// Runs `work` on a new thread. When it finishes, `done` runs on the
    /// thread that calls `poll`.
    pub fn spawn<T: Send + 'static>(
        &self,
        name: &str,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) {
        let (tx, rx): (_, Receiver<T>) = channel();
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = tx.send(work());
            });
        if spawned.is_err() {
            return;
        }
        let mut done = Some(done);
        self.pollers
            .borrow_mut()
            .push(Box::new(move || match rx.try_recv() {
                Ok(v) => {
                    if let Some(d) = done.take() {
                        d(v);
                    }
                    true
                }
                Err(TryRecvError::Empty) => false,
                Err(TryRecvError::Disconnected) => true,
            }));
    }

    /// Adds a poller that is called each tick until it returns true.
    pub fn watch(&self, f: impl FnMut() -> bool + 'static) {
        self.pollers.borrow_mut().push(Box::new(f));
    }

    /// Call from the 10 ms source. Returns how many tasks finished.
    pub fn poll(&self) -> usize {
        // Take the list so callbacks may spawn more tasks.
        let mut list = std::mem::take(&mut *self.pollers.borrow_mut());
        let before = list.len();
        list.retain_mut(|p| !p());
        let finished = before - list.len();
        let mut cur = self.pollers.borrow_mut();
        list.append(&mut cur);
        *cur = list;
        finished
    }

    pub fn pending(&self) -> usize {
        self.pollers.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    fn wait(t: &Tasks) {
        let t0 = Instant::now();
        while t.pending() > 0 && t0.elapsed() < Duration::from_secs(5) {
            t.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn result_arrives_on_the_polling_thread() {
        let t = Tasks::new();
        let got = Rc::new(Cell::new(0));
        let g = got.clone();
        let main = std::thread::current().id();
        t.spawn(
            "t",
            || 41 + 1,
            move |v| {
                assert_eq!(std::thread::current().id(), main);
                g.set(v);
            },
        );
        wait(&t);
        assert_eq!(got.get(), 42);
    }

    #[test]
    fn a_callback_may_spawn_another_task() {
        let t = Rc::new(Tasks::new());
        let got = Rc::new(Cell::new(0));
        let (t2, g) = (t.clone(), got.clone());
        t.spawn(
            "a",
            || 1,
            move |a| {
                let g = g.clone();
                t2.spawn("b", move || a + 1, move |b| g.set(b));
            },
        );
        wait(&t);
        assert_eq!(got.get(), 2);
    }

    #[test]
    fn watchers_run_until_they_finish() {
        let t = Tasks::new();
        let n = Rc::new(Cell::new(0));
        let n2 = n.clone();
        t.watch(move || {
            n2.set(n2.get() + 1);
            n2.get() >= 3
        });
        for _ in 0..5 {
            t.poll();
        }
        assert_eq!(n.get(), 3);
        assert_eq!(t.pending(), 0);
    }
}
