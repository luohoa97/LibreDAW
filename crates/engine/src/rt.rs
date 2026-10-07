// SPDX-License-Identifier: GPL-3.0-or-later
//! Real-time rules for the audio thread (SPEC 3.2): the debug allocator guard
//! and the floating-point mode.

use std::cell::Cell;

/// Per-thread audio-thread state, read by the allocator wrapper.
const RT_OFF: u8 = 0;
/// Any allocation or free aborts the process.
const RT_ABORT: u8 = 1;
/// Any allocation or free is counted instead (tests only).
const RT_COUNT: u8 = 2;

thread_local! {
    // `const` init and no destructor: reading it never allocates.
    static RT_MODE: Cell<u8> = const { Cell::new(RT_OFF) };
    static RT_EVENTS: Cell<u64> = const { Cell::new(0) };
}

/// Marks the current thread as a real-time thread until dropped. In debug
/// builds, any heap allocation or free while a guard is alive aborts.
pub struct RtGuard {
    previous: u8,
}

impl RtGuard {
    pub fn enter() -> Self {
        let previous = RT_MODE.with(Cell::get);
        RT_MODE.with(|m| m.set(previous.max(RT_ABORT)));
        RtGuard { previous }
    }

    /// Like `enter`, but allocations are counted (see `rt_events`) instead of
    /// aborting, so a test can assert on the count.
    pub fn enter_counting() -> Self {
        let previous = RT_MODE.with(Cell::get);
        RT_MODE.with(|m| m.set(RT_COUNT));
        RtGuard { previous }
    }
}

impl Drop for RtGuard {
    fn drop(&mut self) {
        RT_MODE.with(|m| m.set(self.previous));
    }
}

/// Allocations and frees seen on this thread while a counting guard was alive.
pub fn rt_events() -> u64 {
    RT_EVENTS.with(Cell::get)
}

#[cfg(debug_assertions)]
mod guard_alloc {
    use super::{RT_ABORT, RT_COUNT, RT_EVENTS, RT_MODE};
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::io::Write;

    pub struct RtAllocator;

    fn check() {
        match RT_MODE.try_with(|m| m.get()) {
            Ok(RT_ABORT) => {
                RT_MODE.with(|m| m.set(0)); // stderr must not recurse into us
                let _ = std::io::stderr()
                    .write_all(b"libredaw: heap allocation on the real-time thread\n");
                std::process::abort();
            }
            Ok(RT_COUNT) => {
                let _ = RT_EVENTS.try_with(|e| e.set(e.get() + 1));
            }
            _ => {}
        }
    }

    unsafe impl GlobalAlloc for RtAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            check();
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            check();
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            check();
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            check();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOCATOR: guard_alloc::RtAllocator = guard_alloc::RtAllocator;

/// Saved floating-point control state, returned by `enter_rt_fp_mode`.
#[derive(Clone, Copy, Debug)]
pub struct FpMode(u64);

/// Sets flush-to-zero and denormals-are-zero for the calling thread and
/// returns the previous state (x86_64: MXCSR FTZ|DAZ; aarch64: FPCR.FZ).
/// Other architectures: no-op.
pub fn enter_rt_fp_mode() -> FpMode {
    #[cfg(target_arch = "x86_64")]
    {
        let mut csr: u32 = 0;
        // SAFETY: stmxcsr/ldmxcsr only read and write the thread's MXCSR.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
            let new = csr | 0x8040; // FTZ (bit 15) | DAZ (bit 6)
            std::arch::asm!("ldmxcsr [{}]", in(reg) &new, options(nostack));
        }
        FpMode(csr as u64)
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: reads and writes the thread's FPCR; FZ is bit 24.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack));
            std::arch::asm!("msr fpcr, {}", in(reg) fpcr | (1 << 24), options(nomem, nostack));
        }
        FpMode(fpcr)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        FpMode(0)
    }
}

/// Restores the state returned by `enter_rt_fp_mode`.
pub fn restore_fp_mode(previous: FpMode) {
    #[cfg(target_arch = "x86_64")]
    {
        let csr = previous.0 as u32;
        // SAFETY: restores a value previously read from MXCSR.
        unsafe { std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack)) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: restores a value previously read from FPCR.
        unsafe { std::arch::asm!("msr fpcr, {}", in(reg) previous.0, options(nomem, nostack)) };
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = previous;
    }
}

/// Scheduling of the calling thread: `(policy, priority, nice)`. Policy numbers
/// are Linux's: 0 OTHER, 1 FIFO, 2 RR, 3 BATCH, 5 IDLE, 6 DEADLINE.
pub fn thread_sched() -> (i32, i32, i32) {
    #[repr(C)]
    struct SchedParam {
        priority: i32,
    }
    unsafe extern "C" {
        fn sched_getscheduler(pid: i32) -> i32;
        fn sched_getparam(pid: i32, param: *mut SchedParam) -> i32;
        fn getpriority(which: i32, who: u32) -> i32;
    }
    let mut p = SchedParam { priority: -1 };
    // SAFETY: plain libc queries about the calling thread (pid 0 / who 0);
    // `p` is a valid out-pointer. No allocation.
    unsafe {
        let policy = sched_getscheduler(0);
        if sched_getparam(0, &mut p) != 0 {
            p.priority = -1;
        }
        (policy, p.priority, getpriority(0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn fp_mode_flushes_denormals_and_restores() {
        let denormal = std::hint::black_box(f32::MIN_POSITIVE / 4.0);
        assert!(denormal != 0.0);
        let prev = enter_rt_fp_mode();
        assert_eq!(
            std::hint::black_box(denormal) * std::hint::black_box(1.0),
            0.0
        );
        restore_fp_mode(prev);
        assert!(std::hint::black_box(denormal) * std::hint::black_box(1.0) != 0.0);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn counting_guard_sees_allocations() {
        let before = rt_events();
        let guard = RtGuard::enter_counting();
        let v = std::hint::black_box(vec![1u8; 64]);
        drop(v);
        drop(guard);
        assert!(rt_events() >= before + 2, "alloc and free must be counted");
        let after = rt_events();
        drop(std::hint::black_box(vec![1u8; 64]));
        assert_eq!(rt_events(), after, "not counted outside a guard");
    }
}
