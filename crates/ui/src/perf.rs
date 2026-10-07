// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing counters (`LIBREDAW_DEBUG`): how often each custom widget
//! rebuilds its cached render nodes, and how long `snapshot()` takes. The
//! numbers print to stderr every two seconds while a widget draws.
//!
//! `snapshot()` time is the time to record the render node tree, which is
//! the part our code controls. GPU time is not included.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// Whether `LIBREDAW_DEBUG` is set.
pub fn enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIBREDAW_DEBUG").is_some())
}

/// Counters of one widget kind.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counter {
    pub frames: u64,
    pub rebuilds: u64,
    pub total: Duration,
    pub max: Duration,
    /// Total time of rebuilds.
    pub rebuild_total: Duration,
}

impl Counter {
    pub fn avg_frame_us(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1e6 / self.frames as f64
        }
    }

    pub fn line(&self, name: &str) -> String {
        format!(
            "perf: {name}: frames={} rebuilds={} frame avg={:.0}us max={:.0}us rebuild avg={:.0}us",
            self.frames,
            self.rebuilds,
            self.avg_frame_us(),
            self.max.as_secs_f64() * 1e6,
            if self.rebuilds == 0 {
                0.0
            } else {
                self.rebuild_total.as_secs_f64() * 1e6 / self.rebuilds as f64
            }
        )
    }
}

struct Registry {
    items: Vec<(&'static str, Counter)>,
    last_print: Instant,
}

thread_local! {
    static REG: RefCell<Registry> = RefCell::new(Registry {
        items: Vec::new(),
        last_print: Instant::now(),
    });
}

fn with<R>(name: &'static str, f: impl FnOnce(&mut Counter) -> R) -> R {
    REG.with(|r| {
        let mut r = r.borrow_mut();
        let i = match r.items.iter().position(|(n, _)| *n == name) {
            Some(i) => i,
            None => {
                r.items.push((name, Counter::default()));
                r.items.len() - 1
            }
        };
        f(&mut r.items[i].1)
    })
}

/// Marks the start of a `snapshot()`.
pub struct Frame {
    name: &'static str,
    start: Instant,
}

pub fn frame(name: &'static str) -> Frame {
    Frame {
        name,
        start: Instant::now(),
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let d = self.start.elapsed();
        with(self.name, |c| {
            c.frames += 1;
            c.total += d;
            c.max = c.max.max(d);
        });
        maybe_print();
    }
}

/// Records that a cache layer was rebuilt and how long that took.
pub fn rebuilt(name: &'static str, took: Duration) {
    with(name, |c| {
        c.rebuilds += 1;
        c.rebuild_total += took;
    });
}

/// A copy of the counters (tests and the debug report).
pub fn counter(name: &'static str) -> Counter {
    with(name, |c| c.clone())
}

fn maybe_print() {
    if !enabled() {
        return;
    }
    REG.with(|r| {
        let mut r = r.borrow_mut();
        if r.last_print.elapsed() < Duration::from_secs(2) {
            return;
        }
        r.last_print = Instant::now();
        for (n, c) in r.items.iter_mut() {
            if c.frames > 0 || c.rebuilds > 0 {
                eprintln!("{}", c.line(n));
                *c = Counter::default();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_add_up() {
        {
            let _f = frame("t-frames");
        }
        {
            let _f = frame("t-frames");
        }
        rebuilt("t-frames", Duration::from_micros(100));
        let c = counter("t-frames");
        assert_eq!(c.frames, 2);
        assert_eq!(c.rebuilds, 1);
        assert!(c.line("x").contains("rebuilds=1"));
    }
}
