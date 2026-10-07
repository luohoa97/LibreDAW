// SPDX-License-Identifier: GPL-3.0-or-later
//! Swap protocol (SPEC 4.2), slot generations (4.1) and control values (4.3).

mod common;

use common::*;
use engine::compile;
use engine::rt::{RtGuard, rt_events};
use protocol::engine::{ChannelSlot, MixControl, channel_control, param_index};
use protocol::model::SynthParam;
use std::collections::BTreeSet;

fn proj() -> protocol::model::Project {
    project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(1, 16, &[(1, vec![(1, 0, 3840, 69, 127)])])],
    )
}

/// A compiled state tagged through its sample rate.
fn tagged(r: &Rig, tag: f64) -> Box<engine::Compiled> {
    compile(&proj(), &r.slots, tag)
}

fn tag_of(c: &engine::Compiled) -> u32 {
    c.sample_rate as u32
}

fn process_counting(r: &mut Rig) {
    let mut l = [0.0f32; 64];
    let mut rr = [0.0f32; 64];
    let before = rt_events();
    let g = RtGuard::enter_counting();
    r.rt.process_planar(&mut l, &mut rr);
    drop(g);
    assert_eq!(rt_events(), before, "the audio thread allocated or freed");
}

/// Every state ever created is in exactly one place afterwards.
#[test]
fn swap_never_frees_or_strands_a_state_even_with_full_rings() {
    let mut r = rig(&proj(), 48000.0, true);
    // the initial state carries tag 48000
    let mut all: BTreeSet<u32> = BTreeSet::from([48000]);
    let mut retired: Vec<u32> = Vec::new();
    let mut next_tag = 100u32;
    let mut pending_not_pushed: Vec<Box<engine::Compiled>> = Vec::new();

    // Retire ring capacity is 4: swap five states in without draining.
    for _ in 0..5 {
        let c = tagged(&r, next_tag as f64);
        all.insert(next_tag);
        next_tag += 1;
        match r.ui.state.push(c) {
            Ok(()) => {}
            Err(rtrb::PushError::Full(c)) => pending_not_pushed.push(c),
        }
        process_counting(&mut r);
    }
    // Some swaps were refused because the retire ring was full; none lost.
    assert!(
        r.ui.retired.slots() <= protocol::consts::RETIRE_RING_CAP,
        "retire ring overfilled"
    );
    // Drain on the disposal side and let the audio thread continue.
    for _ in 0..10 {
        while let Ok(c) = r.ui.retired.pop() {
            retired.push(tag_of(&c));
        }
        while let Some(c) = pending_not_pushed.pop() {
            if let Err(rtrb::PushError::Full(c)) = r.ui.state.push(c) {
                pending_not_pushed.push(c);
                break;
            }
        }
        process_counting(&mut r);
    }
    while let Ok(c) = r.ui.retired.pop() {
        retired.push(tag_of(&c));
    }
    assert!(pending_not_pushed.is_empty());
    let mut seen: BTreeSet<u32> = retired.iter().copied().collect();
    assert_eq!(seen.len(), retired.len(), "a state was retired twice");
    seen.insert(tag_of(r.rt.compiled().unwrap()));
    // a state still waiting in the state ring counts too
    assert!(r.ui.state.slots() == protocol::consts::STATE_RING_CAP);
    assert_eq!(
        seen, all,
        "every state is current or retired, none stranded"
    );
    assert_eq!(
        tag_of(r.rt.compiled().unwrap()),
        next_tag - 1,
        "newest wins"
    );
}

#[test]
fn a_full_retire_ring_defers_the_swap_without_popping() {
    let mut r = rig(&proj(), 48000.0, false);
    // The retire ring (capacity 4) holds at most 3 at rest: a swap needs
    // pending + 1 free slots.
    for i in 0..3u32 {
        assert!(r.ui.state.push(tagged(&r, 10.0 + i as f64)).is_ok());
        process_counting(&mut r);
    }
    assert_eq!(r.ui.retired.slots(), 3);
    assert_eq!(tag_of(r.rt.compiled().unwrap()), 12);
    // one pending state needs 2 free slots; there is 1
    assert!(r.ui.state.push(tagged(&r, 13.0)).is_ok());
    process_counting(&mut r);
    assert_eq!(tag_of(r.rt.compiled().unwrap()), 12, "deferred");
    assert_eq!(
        r.ui.state.slots(),
        1,
        "the state is still queued, not popped"
    );
    // two pending need 3 free
    assert!(r.ui.state.push(tagged(&r, 14.0)).is_ok());
    r.ui.retired.pop().unwrap();
    process_counting(&mut r);
    assert_eq!(tag_of(r.rt.compiled().unwrap()), 12, "2 free, 3 needed");
    assert_eq!(r.ui.state.slots(), 0, "both still queued");
    r.ui.retired.pop().unwrap();
    process_counting(&mut r);
    assert_eq!(tag_of(r.rt.compiled().unwrap()), 14, "newest wins");
    let mut tags = Vec::new();
    while let Ok(c) = r.ui.retired.pop() {
        tags.push(tag_of(&c));
    }
    tags.sort();
    assert_eq!(
        tags,
        vec![11, 12, 13],
        "11 was waiting; 13 skipped; 12 was current"
    );
}

#[test]
fn edits_that_keep_slot_generations_keep_sounding_voices() {
    let r0 = |play| rig(&proj(), 48000.0, play);
    // reference run without any swap
    let mut a = r0(true);
    let (la, _) = a.run(24000, 256);
    // same run with a recompile in the middle that adds a later note
    let mut b = r0(true);
    let (mut l1, _) = b.run(9000, 256);
    assert_eq!(b.rt.active_voices(ChannelSlot(0)), 1);
    let edited = project(
        120.0,
        vec![],
        vec![synth_channel(1, 0, tone_params())],
        vec![pattern(
            1,
            16,
            &[(1, vec![(1, 0, 3840, 69, 127), (2, 3000, 100, 72, 100)])],
        )],
    );
    assert!(b.ui.state.push(compile(&edited, &b.slots, 48000.0)).is_ok());
    let (l2, _) = b.run(15000, 256);
    assert_eq!(
        b.rt.active_voices(ChannelSlot(0)),
        1,
        "voice survived the swap"
    );
    l1.extend(l2);
    assert_eq!(la, l1, "output is identical: nothing was reset");
}

#[test]
fn a_new_slot_generation_resets_the_slot_and_releases_its_notes() {
    let mut r = rig(&proj(), 48000.0, true);
    r.run(9000, 256);
    assert_eq!(r.rt.active_voices(ChannelSlot(0)), 1);
    r.slots.bump_channel(protocol::ids::ChannelId(1));
    assert!(r.ui.state.push(compile(&proj(), &r.slots, 48000.0)).is_ok());
    let n_off_before = r.rt.trace().iter().filter(|e| !e.on).count();
    r.run(64, 64);
    assert_eq!(r.rt.active_voices(ChannelSlot(0)), 0, "voices reset");
    assert_eq!(r.rt.live_notes(ChannelSlot(0)), 0);
    assert_eq!(
        r.rt.trace().iter().filter(|e| !e.on).count(),
        n_off_before + 1
    );
}

#[test]
fn control_and_param_changes_apply_without_a_recompile() {
    let mut r = rig(&proj(), 48000.0, true);
    let id_before = r.rt.compiled().unwrap() as *const _ as usize;
    r.run(4800, 256); // settle
    let (l, _) = r.run(4800, 256);
    let full = rms(&l);
    assert!(full > 0.1, "{full}");

    // volume -6.02 dB halves the level once the 10 ms ramp is done
    let ctl = &r.shared.controls;
    ctl.set(
        channel_control(ChannelSlot(0), MixControl::VolumeDb),
        -6.0206,
    );
    r.run(960, 256);
    let (l, _) = r.run(4800, 256);
    let half = rms(&l);
    assert!((half / full - 0.5).abs() < 0.01, "{}", half / full);

    // cutoff far below the 440 Hz tone: nearly silent
    r.shared.params.set(
        param_index(ChannelSlot(0), SynthParam::CutoffHz.index()),
        40.0,
    );
    r.run(2000, 256);
    let (l, _) = r.run(4800, 256);
    assert!(rms(&l) < half * 0.05, "{} vs {}", rms(&l), half);

    assert_eq!(
        id_before,
        r.rt.compiled().unwrap() as *const _ as usize,
        "no recompile happened"
    );
    assert!(r.ui.retired.pop().is_err(), "nothing was swapped");
}
