// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effects (SPEC 15.5, 17.2): typed pools allocated when the
//! `Runtime` is built, one per effect kind, with fixed counts from
//! `protocol::consts`. A pool entry belongs to an insert (by instance id,
//! through `Slots`); when `Compiled` assigns it a new owner the entry is
//! reset on the audio thread and its input fades in over 256 frames.

pub mod delay;
pub mod dynamics;
pub mod eq;
pub mod reverb;
pub mod saturator;

use crate::compiled::{Compiled, InsertC};
use delay::Delay;
use dynamics::{Compressor, Limiter};
use eq::Eq;
use protocol::beats::BuiltinFxKind;
use protocol::consts::{
    FX_PARAMS_PER_INSERT, FX_POOL_COMPRESSOR, FX_POOL_DELAY, FX_POOL_EQ, FX_POOL_LIMITER,
    FX_POOL_REVERB, FX_POOL_SATURATOR,
};
use protocol::engine::SlotGen;
use reverb::Reverb;
use saturator::Saturator;

/// Frames over which a newly assigned effect's input fades in (17.2).
pub const FADE_IN_FRAMES: u32 = 256;

/// Number of effect kinds (one pool each).
pub const KINDS: usize = 6;

/// Every kind, in pool order.
pub const ALL_KINDS: [BuiltinFxKind; KINDS] = [
    BuiltinFxKind::Eq,
    BuiltinFxKind::Compressor,
    BuiltinFxKind::Saturator,
    BuiltinFxKind::Reverb,
    BuiltinFxKind::Delay,
    BuiltinFxKind::Limiter,
];

/// Pool index of a kind.
pub fn kind_index(k: BuiltinFxKind) -> usize {
    match k {
        BuiltinFxKind::Eq => 0,
        BuiltinFxKind::Compressor => 1,
        BuiltinFxKind::Saturator => 2,
        BuiltinFxKind::Reverb => 3,
        BuiltinFxKind::Delay => 4,
        BuiltinFxKind::Limiter => 5,
    }
}

/// Entries in the pool of `k`.
pub fn pool_size(k: BuiltinFxKind) -> usize {
    match k {
        BuiltinFxKind::Eq => FX_POOL_EQ,
        BuiltinFxKind::Compressor => FX_POOL_COMPRESSOR,
        BuiltinFxKind::Saturator => FX_POOL_SATURATOR,
        BuiltinFxKind::Reverb => FX_POOL_REVERB,
        BuiltinFxKind::Delay => FX_POOL_DELAY,
        BuiltinFxKind::Limiter => FX_POOL_LIMITER,
    }
}

/// Ramps `l` and `r` from silence to unity over the frames `fade` still
/// holds, counting it down. A finished fade costs nothing.
#[inline]
pub fn fade_in(fade: &mut u32, l: &mut [f32], r: &mut [f32]) {
    if *fade == 0 {
        return;
    }
    for i in 0..l.len() {
        if *fade == 0 {
            break;
        }
        let g = 1.0 - *fade as f32 / FADE_IN_FRAMES as f32;
        l[i] *= g;
        r[i] *= g;
        *fade -= 1;
    }
}

pub struct FxPools {
    eq: Vec<Eq>,
    comp: Vec<Compressor>,
    sat: Vec<Saturator>,
    reverb: Vec<Reverb>,
    delay: Vec<Delay>,
    limiter: Vec<Limiter>,
    /// Generation of each entry as last seen, per kind.
    seen: [Vec<SlotGen>; KINDS],
}

impl FxPools {
    /// Allocates every pool entry, sized for `sr`. Off the audio thread.
    pub fn new(sr: f64) -> FxPools {
        FxPools {
            eq: vec![Eq::new(sr); FX_POOL_EQ],
            comp: vec![Compressor::new(sr); FX_POOL_COMPRESSOR],
            sat: vec![Saturator::new(sr); FX_POOL_SATURATOR],
            reverb: vec![Reverb::new(sr); FX_POOL_REVERB],
            delay: vec![Delay::new(sr); FX_POOL_DELAY],
            limiter: vec![Limiter::new(sr); FX_POOL_LIMITER],
            seen: [
                vec![0; FX_POOL_EQ],
                vec![0; FX_POOL_COMPRESSOR],
                vec![0; FX_POOL_SATURATOR],
                vec![0; FX_POOL_REVERB],
                vec![0; FX_POOL_DELAY],
                vec![0; FX_POOL_LIMITER],
            ],
        }
    }

    /// Resets every entry whose owner changed in `c`. Constant time per
    /// entry: the delay lines are not cleared, they just forget what they
    /// held (see `Delay` and `Reverb`).
    pub fn apply_generations(&mut self, c: &Compiled) {
        for k in 0..KINDS {
            for i in 0..self.seen[k].len() {
                let g = c.fx_gen[k].get(i).copied().unwrap_or(0);
                if g != self.seen[k][i] {
                    self.seen[k][i] = g;
                    match k {
                        0 => self.eq[i].reset(),
                        1 => self.comp[i].reset(),
                        2 => self.sat[i].reset(),
                        3 => self.reverb[i].reset(),
                        4 => self.delay[i].reset(),
                        _ => self.limiter[i].reset(),
                    }
                }
            }
        }
    }

    /// Runs one insert in place. `key` is the sidechain tap for a
    /// compressor that has one.
    pub fn process(
        &mut self,
        ins: &InsertC,
        p: &[f32; FX_PARAMS_PER_INSERT],
        bpm: f64,
        l: &mut [f32],
        r: &mut [f32],
        key: Option<(&[f32], &[f32])>,
    ) {
        match *ins {
            InsertC::Eq { pool } => self.eq[pool as usize].process(p, l, r),
            InsertC::Compressor { pool, .. } => self.comp[pool as usize].process(p, l, r, key),
            InsertC::Saturator { pool, curve } => self.sat[pool as usize].process(curve, p, l, r),
            InsertC::Reverb { pool } => self.reverb[pool as usize].process(p, l, r),
            InsertC::Delay { pool, ping_pong } => {
                self.delay[pool as usize].process(ping_pong, bpm, p, l, r)
            }
            InsertC::Limiter { pool } => self.limiter[pool as usize].process(p, l, r),
            InsertC::Clap | InsertC::Empty => {}
        }
    }

    pub fn compressor_gain_reduction_db(&self, pool: usize) -> f32 {
        self.comp[pool].gain_reduction_db()
    }
}
