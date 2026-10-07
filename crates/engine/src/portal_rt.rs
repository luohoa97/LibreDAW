// SPDX-License-Identifier: GPL-3.0-or-later
//! Real-time priority through the XDG Realtime portal (Flatpak).
//!
//! Inside a Flatpak sandbox the direct rtkit request that cpal makes fails
//! (no system bus, and rtkit sees host pids), so the callback thread stays
//! `SCHED_OTHER`. The portal `org.freedesktop.portal.Realtime` proxies to
//! rtkit and maps the pid namespace. This module asks it to promote the
//! audio callback thread: the callback only publishes its thread id and
//! wakes a helper thread, and the helper does all D-Bus work (never on the
//! audio thread). The outcome is kept as text for the probe and logs.

use dbus::{BusType, Connection, Message, MessageItem, Props};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::{JoinHandle, Thread};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Realtime";
const TIMEOUT_MS: i32 = 3000;
/// Priority asked for, capped at the portal's `MaxRealtimePriority`.
const WANTED_PRIORITY: i64 = 10;

/// Whether the process runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Outcome text shared with the engine (`Engine::realtime_report`).
pub type Report = Arc<Mutex<String>>;

pub fn new_report() -> Report {
    Arc::new(Mutex::new(if in_flatpak() {
        "flatpak: portal request pending".to_string()
    } else {
        "direct rtkit via cpal (not in a Flatpak)".to_string()
    }))
}

fn set(report: &Report, text: String) {
    *report.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

pub struct Promoter {
    tid: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    thread: Thread,
    join: Option<JoinHandle<()>>,
}

impl Promoter {
    /// Starts the helper thread when running in a Flatpak. `frames` is the
    /// buffer size (0 = unknown) and `rate` the sample rate, for the
    /// `RLIMIT_RTTIME` budget.
    pub fn spawn(rate: u32, frames: u32, report: Report) -> Option<Promoter> {
        if !in_flatpak() {
            return None;
        }
        let tid = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (tid.clone(), stop.clone());
        let r = report.clone();
        let join = std::thread::Builder::new()
            .name("libredaw-rt-portal".into())
            .spawn(move || {
                loop {
                    if s.load(Relaxed) {
                        return;
                    }
                    let id = t.swap(0, Relaxed);
                    if id != 0 {
                        let text = match promote(id, rate, frames) {
                            Ok(text) => text,
                            Err(e) => format!("flatpak: portal realtime failed: {e}"),
                        };
                        set(&r, text);
                        continue;
                    }
                    std::thread::park();
                }
            });
        match join {
            Ok(j) => Some(Promoter {
                tid,
                stop,
                thread: j.thread().clone(),
                join: Some(j),
            }),
            Err(e) => {
                set(&report, format!("flatpak: cannot start portal thread: {e}"));
                None
            }
        }
    }

    /// Call on the audio callback thread (once). Publishes the thread id
    /// and wakes the helper; no allocation, no D-Bus.
    pub fn request_from_callback(&self) {
        // SAFETY: gettid has no arguments and cannot fail.
        let id = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
        self.tid.store(id, Relaxed);
        self.thread.unpark();
    }
}

impl Drop for Promoter {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        self.thread.unpark();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn item_as_i64(i: MessageItem) -> Result<i64, String> {
    match i {
        MessageItem::Int32(v) => Ok(v as i64),
        MessageItem::Int64(v) => Ok(v),
        other => Err(format!("property is not an integer ({other:?})")),
    }
}

/// Asks the portal to make thread `tid` real-time. Returns the report text.
fn promote(tid: u32, rate: u32, frames: u32) -> Result<String, String> {
    let c =
        Connection::get_private(BusType::Session).map_err(|e| format!("no session bus ({e})"))?;
    let props = Props::new(&c, DEST, PATH, IFACE, TIMEOUT_MS);
    let max_prio = props
        .get("MaxRealtimePriority")
        .map_err(|e| format!("portal has no Realtime interface ({e:?})"))
        .and_then(item_as_i64)?;
    let max_rttime = props
        .get("RTTimeUSecMax")
        .map_err(|e| format!("cannot read RTTimeUSecMax ({e:?})"))
        .and_then(item_as_i64)?;
    if max_prio < 1 || max_rttime < 1 {
        return Err(format!(
            "portal refuses realtime (max priority {max_prio}, rttime {max_rttime} us)"
        ));
    }

    // rtkit insists on a finite RLIMIT_RTTIME at or below its maximum. The
    // soft limit is the buffer time (a callback that blocks resets it).
    let budget_us = if frames > 0 {
        frames as u64 * 1_000_000 / rate.max(1) as u64
    } else {
        50_000
    }
    .max(1000);
    let hard = max_rttime as u64;
    let soft = budget_us.min(hard);
    let mut previous = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain libc calls with valid pointers to local structs.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_RTTIME, &mut previous) != 0 {
            return Err("getrlimit(RLIMIT_RTTIME) failed".into());
        }
        let want = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        if libc::setrlimit(libc::RLIMIT_RTTIME, &want) != 0 {
            return Err(format!(
                "setrlimit(RLIMIT_RTTIME) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    let prio = max_prio.min(WANTED_PRIORITY) as u32;
    let pid = std::process::id() as u64;
    let mut m = Message::new_method_call(DEST, PATH, IFACE, "MakeThreadRealtimeWithPID")
        .map_err(|e| format!("cannot build the portal call ({e})"))?;
    m.append_items(&[pid.into(), (tid as u64).into(), prio.into()]);
    match c.send_with_reply_and_block(m, TIMEOUT_MS) {
        Ok(_) => Ok(format!(
            "flatpak: portal granted realtime (priority {prio}, rttime {soft} us)"
        )),
        Err(e) => {
            // SAFETY: restoring the limits read above.
            unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &previous) };
            Err(format!(
                "{} {}",
                e.name().unwrap_or("error"),
                e.message().unwrap_or("")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_flatpak_nothing_is_requested() {
        if in_flatpak() {
            return;
        }
        assert!(Promoter::spawn(48000, 256, new_report()).is_none());
        let r = new_report();
        assert!(r.lock().unwrap().contains("not in a Flatpak"));
    }

    #[test]
    fn a_failed_promotion_reports_a_reason_instead_of_panicking() {
        // A thread id that does not exist: whether or not a session bus
        // and portal are present, the answer is an `Ok` text or a
        // non-empty `Err` reason, never a panic. (A successful rlimit
        // change lowers the hard limit for good, as rtkit requires.)
        if let Err(e) = promote(0x7fff_fff0, 48000, 256) {
            assert!(!e.is_empty());
        }
    }
}
