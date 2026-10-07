// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use protocol::edit::NewInstrument;
use protocol::model::{SynthParam, SynthParams};

/// xorshift64*, so tests need no random-number crate and are repeatable.
pub(crate) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { (self.next() >> 11) % n }
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    pub fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        if v.is_empty() {
            None
        } else {
            Some(v[self.below(v.len() as u64) as usize])
        }
    }
}

fn ok(doc: &Document, e: Edit) -> (Document, Vec<u32>) {
    apply(doc, &e).unwrap_or_else(|err| panic!("edit failed: {err}: {e:?}"))
}

fn synth() -> NewInstrument {
    NewInstrument::Synth {
        params: SynthParams::default(),
    }
}

/// A document with one pattern (16 steps) and one synth channel on master.
fn base() -> (Document, ChannelId, PatternId) {
    let d = Document::new();
    let (d, c) = ok(
        &d,
        Edit::AddChannel {
            name: "Lead".into(),
            instrument: synth(),
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let (d, p) = ok(
        &d,
        Edit::AddPattern {
            name: "A".into(),
            length_steps: 16,
        },
    );
    (d, ChannelId(c[0]), PatternId(p[0]))
}

fn step_on(d: &Document, p: PatternId, c: ChannelId, step: u8) -> Document {
    ok(
        d,
        Edit::SetStep {
            pattern: p,
            channel: c,
            step,
            on: true,
            vel: None,
        },
    )
    .0
}

fn steps_on(d: &Document, p: PatternId, c: ChannelId) -> Vec<u8> {
    let pat = d.project.pattern(p).unwrap();
    let root = d.project.channel(c).unwrap().root_key;
    pat.notes_of(c)
        .iter()
        .filter(|n| n.is_step_note(root, pat))
        .map(|n| (n.start / pat.step_ticks) as u8)
        .collect()
}

fn row_editable(d: &Document, p: PatternId, c: ChannelId) -> bool {
    let pat = d.project.pattern(p).unwrap();
    let root = d.project.channel(c).unwrap().root_key;
    pat.notes_of(c).iter().all(|n| n.is_step_note(root, pat))
}

fn assert_saves(d: &Document) {
    validate(&d.project).expect("validates");
    protocol::format::emit(&d.project, d.next_id).expect("emits");
}

#[test]
fn new_document_is_valid_and_empty() {
    let d = Document::new();
    assert_saves(&d);
    assert_eq!(d.revision, 0);
    assert_eq!(d.next_id, FIRST_ID);
}

#[test]
fn apply_bumps_revision_once_per_batch() {
    let (d, c, _) = base();
    let rev = d.revision;
    let (d2, _) = apply_batch(
        &d,
        &[
            Edit::RenameChannel {
                channel: c,
                name: "x".into(),
            },
            Edit::SetTempo { bpm: 90.0 },
        ],
    )
    .unwrap();
    assert_eq!(d2.revision, rev + 1);
    assert_eq!(d2.project.tempo_bpm, 90.0);
}

#[test]
fn failed_batch_applies_nothing() {
    let (d, c, p) = base();
    let err = apply_batch(
        &d,
        &[
            Edit::SetTempo { bpm: 90.0 },
            Edit::AddNotes {
                pattern: p,
                channel: c,
                notes: vec![NewNote {
                    start: 0,
                    len: 0,
                    key: 60,
                    vel: 100,
                }],
            },
        ],
    );
    assert!(err.is_err());
    assert_eq!(d.project.tempo_bpm, 120.0);
}

#[test]
fn set_tempo_range_checked() {
    let d = Document::new();
    for bad in [0.0, 19.9, 1000.0, f64::NAN, f64::INFINITY] {
        assert!(apply(&d, &Edit::SetTempo { bpm: bad }).is_err(), "{bad}");
    }
    assert!(apply(&d, &Edit::SetTempo { bpm: 20.0 }).is_ok());
    assert!(apply(&d, &Edit::SetTempo { bpm: 999.0 }).is_ok());
}

#[test]
fn time_sig_and_metronome() {
    let d = Document::new();
    assert!(apply(&d, &Edit::SetTimeSigNum { num: 0 }).is_err());
    assert!(apply(&d, &Edit::SetTimeSigNum { num: 17 }).is_err());
    let (d, _) = ok(&d, Edit::SetTimeSigNum { num: 7 });
    assert_eq!(d.project.time_sig_num, 7);
    let (d, _) = ok(
        &d,
        Edit::SetMetronome {
            enabled: true,
            gain_db: -12.0,
        },
    );
    assert!(d.project.metronome.enabled);
    assert!(
        apply(
            &d,
            &Edit::SetMetronome {
                enabled: true,
                gain_db: 13.0
            }
        )
        .is_err()
    );
}

#[test]
fn add_channel_checks_track_and_limits() {
    let d = Document::new();
    let e = Edit::AddChannel {
        name: "x".into(),
        instrument: synth(),
        root_key: 60,
        track: TrackId(99),
    };
    assert!(matches!(
        apply(&d, &e),
        Err(EditError::NotFound { what, id: 99 }) if what == "track"
    ));
    let mut d = Document::new();
    for i in 0..MAX_CHANNELS {
        d = ok(
            &d,
            Edit::AddChannel {
                name: format!("c{i}"),
                instrument: synth(),
                root_key: 60,
                track: TrackId::MASTER,
            },
        )
        .0;
    }
    let e = Edit::AddChannel {
        name: "one too many".into(),
        instrument: synth(),
        root_key: 60,
        track: TrackId::MASTER,
    };
    assert!(matches!(
        apply(&d, &e),
        Err(EditError::Invalid {
            reason: ValidationError::TooMany { .. }
        })
    ));
    assert_saves(&d);
}

#[test]
fn add_clap_channel_creates_channel_then_instance_id() {
    let d = Document::new();
    let (d, created) = ok(
        &d,
        Edit::AddChannel {
            name: "Surge".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "org.surge-synth-team.surge-xt".into(),
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    assert_eq!(created.len(), 2);
    assert_eq!(created[1], created[0] + 1);
    match &d.project.channels[0].instrument {
        Instrument::Clap(r) => assert_eq!(r.instance.0, created[1]),
        _ => panic!("not clap"),
    }
    assert_saves(&d);
}

#[test]
fn remove_channel_drops_its_notes() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 0);
    assert_eq!(d.project.note_count(), 1);
    let (d, _) = ok(&d, Edit::RemoveChannel { channel: c });
    assert_eq!(d.project.note_count(), 0);
    assert!(d.project.patterns[0].notes.is_empty());
    assert!(apply(&d, &Edit::RemoveChannel { channel: c }).is_err());
    assert_saves(&d);
}

#[test]
fn mix_values_checked() {
    let (d, c, _) = base();
    for v in [
        MixValue::VolumeDb(13.0),
        MixValue::VolumeDb(f64::NAN),
        MixValue::Pan(1.5),
        MixValue::Pan(-1.01),
    ] {
        assert!(
            apply(
                &d,
                &Edit::SetChannelMix {
                    channel: c,
                    value: v
                }
            )
            .is_err(),
            "{v:?}"
        );
        assert!(
            apply(
                &d,
                &Edit::SetTrackMix {
                    track: TrackId::MASTER,
                    value: v
                }
            )
            .is_err()
        );
    }
    let (d, _) = ok(
        &d,
        Edit::SetChannelMix {
            channel: c,
            value: MixValue::Mute(true),
        },
    );
    assert!(d.project.channels[0].mix.mute);
    let (d, _) = ok(
        &d,
        Edit::SetTrackMix {
            track: TrackId::MASTER,
            value: MixValue::Solo(true),
        },
    );
    assert!(d.project.tracks[0].mix.solo);
}

#[test]
fn synth_param_and_wave() {
    let (d, c, _) = base();
    let (d, _) = ok(
        &d,
        Edit::SetSynthParam {
            channel: c,
            param: SynthParam::CutoffHz,
            value: 500.0,
        },
    );
    match &d.project.channels[0].instrument {
        Instrument::Synth(s) => assert_eq!(s.cutoff_hz, 500.0),
        _ => unreachable!(),
    }
    assert!(
        apply(
            &d,
            &Edit::SetSynthParam {
                channel: c,
                param: SynthParam::CutoffHz,
                value: 5.0
            }
        )
        .is_err()
    );
    let (d, _) = ok(
        &d,
        Edit::SetSynthWave {
            channel: c,
            osc: 2,
            wave: Wave::Sine,
        },
    );
    match &d.project.channels[0].instrument {
        Instrument::Synth(s) => assert_eq!(s.osc2.wave, Wave::Sine),
        _ => unreachable!(),
    }
    for osc in [0u8, 3] {
        assert!(matches!(
            apply(
                &d,
                &Edit::SetSynthWave {
                    channel: c,
                    osc,
                    wave: Wave::Saw
                }
            ),
            Err(EditError::BadArgument { .. })
        ));
    }
}

#[test]
fn synth_edits_reject_clap_channels() {
    let d = Document::new();
    let (d, ids) = ok(
        &d,
        Edit::AddChannel {
            name: "p".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "a.b".into(),
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let c = ChannelId(ids[0]);
    assert!(
        apply(
            &d,
            &Edit::SetSynthParam {
                channel: c,
                param: SynthParam::OscMix,
                value: 0.5
            }
        )
        .is_err()
    );
    assert!(
        apply(
            &d,
            &Edit::SetSynthWave {
                channel: c,
                osc: 1,
                wave: Wave::Saw
            }
        )
        .is_err()
    );
}

#[test]
fn patterns_add_rename_remove_and_length() {
    let (d, c, p) = base();
    for (name, len) in [("x", 0u8), ("x", 65), ("", 16)] {
        assert!(
            apply(
                &d,
                &Edit::AddPattern {
                    name: name.into(),
                    length_steps: len
                }
            )
            .is_err()
        );
    }
    let mut d = d;
    for s in [0, 4, 8, 12] {
        d = step_on(&d, p, c, s);
    }
    // Shorten to 8 steps: steps 8 and 12 go away (5.2).
    let (d, _) = ok(
        &d,
        Edit::SetPatternLength {
            pattern: p,
            length_steps: 8,
        },
    );
    assert_eq!(steps_on(&d, p, c), vec![0, 4]);
    // Grow again: they do not come back, nothing else changes.
    let (d, _) = ok(
        &d,
        Edit::SetPatternLength {
            pattern: p,
            length_steps: 16,
        },
    );
    assert_eq!(steps_on(&d, p, c), vec![0, 4]);
    let (d, _) = ok(
        &d,
        Edit::RenamePattern {
            pattern: p,
            name: "B".into(),
        },
    );
    assert_eq!(d.project.patterns[0].name, "B");
    let (d, _) = ok(&d, Edit::RemovePattern { pattern: p });
    assert!(d.project.patterns.is_empty());
    assert_saves(&d);
}

#[test]
fn shortening_removes_notes_starting_at_or_after_the_end_only() {
    let (d, c, p) = base();
    let (d, _) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![
                NewNote {
                    start: 7 * 240 + 100,
                    len: 2000,
                    key: 50,
                    vel: 90,
                },
                NewNote {
                    start: 8 * 240,
                    len: 10,
                    key: 50,
                    vel: 90,
                },
            ],
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetPatternLength {
            pattern: p,
            length_steps: 8,
        },
    );
    let notes = d.project.patterns[0].notes_of(c);
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].start, 7 * 240 + 100);
}

#[test]
fn step_on_off_and_velocity() {
    let (d, c, p) = base();
    let set = |d: &Document, on: bool, vel: Option<u8>| {
        ok(
            d,
            Edit::SetStep {
                pattern: p,
                channel: c,
                step: 3,
                on,
                vel,
            },
        )
    };
    let (d, ids) = set(&d, true, Some(77));
    assert_eq!(ids.len(), 1);
    let n = d.project.patterns[0].notes_of(c)[0];
    assert_eq!(
        (n.start, n.len, n.key, n.vel),
        (3 * 240, 240, 60, 77),
        "step note shape of 5.2"
    );
    // On again is a no-op that may update velocity.
    let (d2, ids) = set(&d, true, Some(50));
    assert!(ids.is_empty());
    assert_eq!(d2.project.patterns[0].notes_of(c).len(), 1);
    assert_eq!(d2.project.patterns[0].notes_of(c)[0].vel, 50);
    let (d3, _) = set(&d, true, None);
    assert_eq!(d3.project.patterns[0].notes_of(c)[0].vel, 77);
    // Off removes the note and the empty channel entry.
    let (d, _) = set(&d, false, None);
    assert!(d.project.patterns[0].notes.is_empty());
    // Off when nothing is there is fine.
    set(&d, false, None);
}

#[test]
fn step_default_velocity() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 0);
    assert_eq!(d.project.patterns[0].notes_of(c)[0].vel, DEFAULT_STEP_VEL);
}

#[test]
fn step_bounds_and_velocity_checked() {
    let (d, c, p) = base();
    for (step, vel) in [(16u8, None), (255, None), (0, Some(0u8)), (0, Some(128))] {
        assert!(
            apply(
                &d,
                &Edit::SetStep {
                    pattern: p,
                    channel: c,
                    step,
                    on: true,
                    vel
                }
            )
            .is_err(),
            "{step} {vel:?}"
        );
    }
}

#[test]
fn step_off_removes_every_root_key_note_at_that_start() {
    let (d, c, p) = base();
    let nn = |len, key| NewNote {
        start: 480,
        len,
        key,
        vel: 90,
    };
    let (d, _) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![nn(100, 60), nn(240, 60), nn(240, 61)],
        },
    );
    let (d, _) = ok(
        &d,
        Edit::SetStep {
            pattern: p,
            channel: c,
            step: 2,
            on: false,
            vel: None,
        },
    );
    let left = d.project.patterns[0].notes_of(c);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].key, 61);
}

#[test]
fn spec_5_2_test_steps_root_key_step_length_row_stays_editable() {
    let (mut d, c, p) = base();
    for s in [0u8, 3, 4, 9, 15] {
        d = step_on(&d, p, c, s);
    }
    let on = vec![0, 3, 4, 9, 15];
    assert_eq!(steps_on(&d, p, c), on);
    assert!(row_editable(&d, p, c));

    // A second pattern with a step too: SetRootKey rewrites all patterns.
    let (d2, p2) = ok(
        &d,
        Edit::AddPattern {
            name: "B".into(),
            length_steps: 8,
        },
    );
    let p2 = PatternId(p2[0]);
    let d2 = step_on(&d2, p2, c, 2);
    let (d3, _) = ok(
        &d2,
        Edit::SetRootKey {
            channel: c,
            key: 36,
        },
    );
    assert_eq!(d3.project.channels[0].root_key, 36);
    assert_eq!(steps_on(&d3, p, c), on);
    assert_eq!(steps_on(&d3, p2, c), vec![2]);
    assert!(row_editable(&d3, p, c));
    assert!(row_editable(&d3, p2, c));

    // Change step length: same steps, new positions.
    let (d4, _) = ok(
        &d3,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 480,
        },
    );
    assert_eq!(d4.project.pattern(p).unwrap().step_ticks, 480);
    assert_eq!(steps_on(&d4, p, c), on);
    assert!(row_editable(&d4, p, c));
    for n in d4.project.pattern(p).unwrap().notes_of(c) {
        assert_eq!(n.len, 480);
        assert_eq!(n.start % 480, 0);
    }
    // Toggling still works afterwards.
    let d5 = step_on(&d4, p, c, 5);
    assert!(steps_on(&d5, p, c).contains(&5));
    // Shrink the step length too.
    let (d6, _) = ok(
        &d5,
        Edit::SetStepTicks {
            pattern: p,
            step_ticks: 120,
        },
    );
    assert_eq!(steps_on(&d6, p, c).len(), 6);
    assert!(row_editable(&d6, p, c));
    assert_saves(&d6);
}

#[test]
fn root_key_leaves_piano_roll_notes_alone() {
    let (d, c, p) = base();
    let d = step_on(&d, p, c, 1);
    let (d, _) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![NewNote {
                start: 5,
                len: 7,
                key: 60,
                vel: 100,
            }],
        },
    );
    assert!(!row_editable(&d, p, c));
    let (d, _) = ok(
        &d,
        Edit::SetRootKey {
            channel: c,
            key: 40,
        },
    );
    let notes = d.project.pattern(p).unwrap().notes_of(c);
    let free: Vec<_> = notes.iter().filter(|n| n.len == 7).collect();
    assert_eq!(free.len(), 1);
    assert_eq!(free[0].key, 60);
    assert_eq!(notes.iter().filter(|n| n.key == 40).count(), 1);
    assert!(
        apply(
            &d,
            &Edit::SetRootKey {
                channel: c,
                key: 128
            }
        )
        .is_err()
    );
}

#[test]
fn step_ticks_range_checked() {
    let (d, _, p) = base();
    for t in [0u32, PPQ * 4 + 1] {
        assert!(
            apply(
                &d,
                &Edit::SetStepTicks {
                    pattern: p,
                    step_ticks: t
                }
            )
            .is_err()
        );
    }
}

#[test]
fn add_notes_validates_every_field() {
    let (d, c, p) = base();
    let good = NewNote {
        start: 0,
        len: 10,
        key: 0,
        vel: 1,
    };
    ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![good],
        },
    );
    let bads = [
        NewNote { len: 0, ..good },
        NewNote { key: 128, ..good },
        NewNote { vel: 0, ..good },
        NewNote { vel: 128, ..good },
        NewNote {
            start: 16 * 240,
            ..good
        },
        NewNote {
            start: 0,
            len: MAX_TICK + 1,
            ..good
        },
        NewNote {
            start: 3,
            len: u32::MAX,
            ..good
        },
    ];
    for b in bads {
        assert!(
            apply(
                &d,
                &Edit::AddNotes {
                    pattern: p,
                    channel: c,
                    notes: vec![good, b]
                }
            )
            .is_err(),
            "{b:?}"
        );
    }
    assert!(
        apply(
            &d,
            &Edit::AddNotes {
                pattern: PatternId(999),
                channel: c,
                notes: vec![good]
            }
        )
        .is_err()
    );
    assert!(
        apply(
            &d,
            &Edit::AddNotes {
                pattern: p,
                channel: ChannelId(999),
                notes: vec![good]
            }
        )
        .is_err()
    );
}

#[test]
fn notes_stay_sorted_and_ids_ascend() {
    let (d, c, p) = base();
    let n = |start, key| NewNote {
        start,
        len: 10,
        key,
        vel: 100,
    };
    let (d, ids) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![n(500, 60), n(100, 70), n(100, 60), n(100, 60)],
        },
    );
    assert_eq!(ids.len(), 4);
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    let notes = d.project.patterns[0].notes_of(c);
    let keys: Vec<_> = notes.iter().map(|n| (n.start, n.key, n.id.0)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn remove_move_resize_velocity() {
    let (d, c, p) = base();
    let nn = |start, key| NewNote {
        start,
        len: 100,
        key,
        vel: 100,
    };
    let (d, ids) = ok(
        &d,
        Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![nn(0, 60), nn(200, 62)],
        },
    );
    let (a, b) = (NoteId(ids[0]), NoteId(ids[1]));
    let (d2, _) = ok(
        &d,
        Edit::MoveNotes {
            pattern: p,
            notes: vec![a, b],
            dt: 50,
            dkey: -2,
        },
    );
    let ns = d2.project.patterns[0].notes_of(c);
    assert_eq!((ns[0].start, ns[0].key), (50, 58));
    assert_eq!((ns[1].start, ns[1].key), (250, 60));
    for (dt, dkey) in [
        (-1, 0),
        (0, -61),
        (0, 68),
        (16 * 240, 0),
        (i64::MAX, 0),
        (i64::MIN, 0),
    ] {
        assert!(
            apply(
                &d,
                &Edit::MoveNotes {
                    pattern: p,
                    notes: vec![a, b],
                    dt,
                    dkey
                }
            )
            .is_err(),
            "{dt} {dkey}"
        );
    }
    // Moving the first past the second reorders them.
    let (d3, _) = ok(
        &d,
        Edit::MoveNotes {
            pattern: p,
            notes: vec![a],
            dt: 400,
            dkey: 0,
        },
    );
    let ns = d3.project.patterns[0].notes_of(c);
    assert_eq!(ns[0].id, b);
    assert_eq!(ns[1].id, a);
    // Resize.
    let (d4, _) = ok(
        &d,
        Edit::ResizeNotes {
            pattern: p,
            notes: vec![a],
            dlen: -99,
        },
    );
    assert_eq!(d4.project.patterns[0].notes_of(c)[0].len, 1);
    for dlen in [-100, i64::MAX, i64::MIN, MAX_TICK as i64] {
        assert!(
            apply(
                &d,
                &Edit::ResizeNotes {
                    pattern: p,
                    notes: vec![a],
                    dlen
                }
            )
            .is_err(),
            "{dlen}"
        );
    }
    // Velocity.
    let (d5, _) = ok(
        &d,
        Edit::SetNoteVelocity {
            pattern: p,
            notes: vec![a, b],
            vel: 1,
        },
    );
    assert!(
        d5.project.patterns[0]
            .notes_of(c)
            .iter()
            .all(|n| n.vel == 1)
    );
    assert!(
        apply(
            &d,
            &Edit::SetNoteVelocity {
                pattern: p,
                notes: vec![a],
                vel: 0
            }
        )
        .is_err()
    );
    // Remove (duplicate ids count once).
    let (d6, _) = ok(
        &d,
        Edit::RemoveNotes {
            pattern: p,
            notes: vec![a, a],
        },
    );
    assert_eq!(d6.project.patterns[0].notes_of(c).len(), 1);
    assert!(matches!(
        apply(
            &d,
            &Edit::RemoveNotes {
                pattern: p,
                notes: vec![a, NoteId(9999)]
            }
        ),
        Err(EditError::NotFound { id: 9999, .. })
    ));
    let (d7, _) = ok(
        &d,
        Edit::RemoveNotes {
            pattern: p,
            notes: vec![a, b],
        },
    );
    assert!(d7.project.patterns[0].notes.is_empty());
}

#[test]
fn note_limits_checked_before_cloning() {
    let (d, c, p) = base();
    // Fill one pattern to its limit directly, so the test does not need
    // 100,000 apply calls.
    let mut proj = (*d.project).clone();
    let pat = Arc::make_mut(&mut proj.patterns[0]);
    pat.length_steps = 64;
    let mut notes: Vec<Note> = (0..MAX_NOTES_PER_PATTERN as u32)
        .map(|i| Note {
            id: NoteId(1000 + i),
            start: i % 1000,
            len: 1,
            key: (i % 128) as u8,
            vel: 100,
        })
        .collect();
    notes.sort_by_key(|n| (n.start, n.key, n.id));
    pat.notes = vec![ChannelNotes { channel: c, notes }];
    let d = Document::from_project(proj, 1000 + MAX_NOTES_PER_PATTERN as u32);
    validate(&d.project).unwrap();
    let r = apply(
        &d,
        &Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![NewNote {
                start: 0,
                len: 1,
                key: 1,
                vel: 1,
            }],
        },
    );
    assert!(matches!(
        r,
        Err(EditError::Invalid {
            reason: ValidationError::TooMany { .. }
        })
    ));
    let r = apply(
        &d,
        &Edit::SetStep {
            pattern: p,
            channel: c,
            step: 1,
            on: true,
            vel: None,
        },
    );
    assert!(r.is_err());
}

#[test]
fn tracks_inserts_and_routing() {
    let (d, c, _) = base();
    let (d, t) = ok(
        &d,
        Edit::AddTrack {
            name: "Drums".into(),
        },
    );
    let t = TrackId(t[0]);
    let (d, _) = ok(
        &d,
        Edit::SetChannelTrack {
            channel: c,
            track: t,
        },
    );
    assert_eq!(d.project.channels[0].track, t);
    assert!(
        apply(
            &d,
            &Edit::SetChannelTrack {
                channel: c,
                track: TrackId(777)
            }
        )
        .is_err()
    );
    let add = |d: &Document, index: u8, id: &str| {
        apply(
            d,
            &Edit::AddInsert {
                track: t,
                index,
                plugin_id: id.into(),
            },
        )
    };
    let (d, ins) = add(&d, 0, "a.b").unwrap();
    let i1 = InstanceId(ins[0]);
    let (d, ins) = add(&d, 0, "c.d").unwrap();
    let i2 = InstanceId(ins[0]);
    let order: Vec<_> = d
        .project
        .track(t)
        .unwrap()
        .inserts
        .iter()
        .map(|Insert::Clap(r)| r.instance)
        .collect();
    assert_eq!(order, vec![i2, i1]);
    assert!(add(&d, 3, "x.y").is_err());
    let (d, _) = ok(
        &d,
        Edit::RemoveInsert {
            track: t,
            instance: i1,
        },
    );
    assert_eq!(d.project.track(t).unwrap().inserts.len(), 1);
    assert!(
        apply(
            &d,
            &Edit::RemoveInsert {
                track: t,
                instance: i1
            }
        )
        .is_err()
    );
    let mut d = d;
    for _ in 0..MAX_INSERTS - 1 {
        d = add(&d, 0, "z.z").unwrap().0;
    }
    assert!(add(&d, 0, "z.z").is_err());
    // Removing the track routes the channel to master.
    let (d, _) = ok(&d, Edit::RemoveTrack { track: t });
    assert_eq!(d.project.channels[0].track, TrackId::MASTER);
    assert!(
        apply(
            &d,
            &Edit::RemoveTrack {
                track: TrackId::MASTER
            }
        )
        .is_err()
    );
    assert_saves(&d);
}

#[test]
fn track_limit() {
    let mut d = Document::new();
    for i in 0..MAX_TRACKS {
        d = ok(
            &d,
            Edit::AddTrack {
                name: format!("t{i}"),
            },
        )
        .0;
    }
    assert_eq!(d.project.tracks.len(), TRACK_SLOTS);
    assert!(apply(&d, &Edit::AddTrack { name: "x".into() }).is_err());
    assert_saves(&d);
}

#[test]
fn rename_validates_names() {
    let (d, c, p) = base();
    for bad in ["", "a\nb", &"x".repeat(MAX_NAME_CHARS + 1)] {
        assert!(
            apply(
                &d,
                &Edit::RenameChannel {
                    channel: c,
                    name: bad.into()
                }
            )
            .is_err()
        );
        assert!(
            apply(
                &d,
                &Edit::RenamePattern {
                    pattern: p,
                    name: bad.into()
                }
            )
            .is_err()
        );
        assert!(
            apply(
                &d,
                &Edit::RenameTrack {
                    track: TrackId::MASTER,
                    name: bad.into()
                }
            )
            .is_err()
        );
    }
    ok(
        &d,
        Edit::RenameTrack {
            track: TrackId::MASTER,
            name: "Main".into(),
        },
    );
}

#[test]
fn plugin_params_and_state() {
    let d = Document::new();
    let (d, ids) = ok(
        &d,
        Edit::AddChannel {
            name: "p".into(),
            instrument: NewInstrument::Clap {
                plugin_id: "a.b".into(),
            },
            root_key: 60,
            track: TrackId::MASTER,
        },
    );
    let inst = InstanceId(ids[1]);
    let set = |d: &Document, id: u32, v: f64| {
        apply(
            d,
            &Edit::SetPluginParam {
                instance: inst,
                param_id: id,
                value: v,
            },
        )
    };
    let (d, _) = set(&d, 9, 0.5).unwrap();
    let (d, _) = set(&d, 3, 0.25).unwrap();
    let (d, _) = set(&d, 9, 0.75).unwrap();
    match &d.project.channels[0].instrument {
        Instrument::Clap(r) => {
            let got: Vec<_> = r.params.iter().map(|p| (p.id, p.value)).collect();
            assert_eq!(got, vec![(3, 0.25), (9, 0.75)]);
        }
        _ => unreachable!(),
    }
    assert!(set(&d, 1, f64::NAN).is_err());
    assert!(
        apply(
            &d,
            &Edit::SetPluginParam {
                instance: InstanceId(12345),
                param_id: 1,
                value: 0.0
            }
        )
        .is_err()
    );
    let (d, _) = ok(
        &d,
        Edit::CommitPluginState {
            instance: inst,
            state_file: state_file_name(inst, 1),
        },
    );
    let bytes: Arc<[u8]> = Arc::from(vec![1u8, 2, 3]);
    let d2 = commit_plugin_state(&d, inst, &state_file_name(inst, 2), bytes, Some("1.2")).unwrap();
    match &d2.project.channels[0].instrument {
        Instrument::Clap(r) => {
            assert_eq!(
                r.state_file.as_deref(),
                Some(format!("{}-2.bin", inst.0).as_str())
            );
            assert_eq!(r.state_bytes.as_deref(), Some(&[1u8, 2, 3][..]));
            assert_eq!(r.plugin_version, "1.2");
            assert_eq!(next_generation(r), 3);
        }
        _ => unreachable!(),
    }
    assert!(
        apply(
            &d,
            &Edit::CommitPluginState {
                instance: inst,
                state_file: "../evil".into()
            }
        )
        .is_err()
    );
    assert_saves(&d2);
}

#[test]
fn state_file_names_round_trip() {
    let n = state_file_name(InstanceId(7), 12);
    assert_eq!(n, "7-12.bin");
    assert_eq!(parse_state_file_name(&n), Some((InstanceId(7), 12)));
    assert_eq!(parse_state_file_name("7-12.tmp"), None);
    assert_eq!(parse_state_file_name("x-1.bin"), None);
}

#[test]
fn with_project_keeps_next_id_monotonic() {
    let (d, _, _) = base();
    let before = d.next_id;
    let d2 = d.with_project(Arc::new(Project::empty()));
    assert_eq!(d2.next_id, before);
    assert_eq!(d2.revision, d.revision + 1);
    let (d3, _) = ok(&d, Edit::AddTrack { name: "t".into() });
    let d4 = d2.with_project(d3.project.clone());
    assert!(d4.next_id >= d3.next_id);
}

#[test]
fn structural_sharing_of_untouched_parts() {
    let (d, c, p) = base();
    let (d, _) = ok(
        &d,
        Edit::AddPattern {
            name: "B".into(),
            length_steps: 16,
        },
    );
    let d = step_on(&d, p, c, 0);
    let d2 = step_on(&d, p, c, 1);
    assert!(Arc::ptr_eq(&d.project.patterns[1], &d2.project.patterns[1]));
    assert!(Arc::ptr_eq(&d.project.channels[0], &d2.project.channels[0]));
    assert!(!Arc::ptr_eq(
        &d.project.patterns[0],
        &d2.project.patterns[0]
    ));
}

// ---------------------------------------------------------------------------
// Property-style tests

struct Ids {
    ch: Vec<ChannelId>,
    pa: Vec<PatternId>,
    tr: Vec<TrackId>,
    inst: Vec<InstanceId>,
}

fn ids_of(d: &Document) -> Ids {
    let mut inst = Vec::new();
    for c in &d.project.channels {
        if let Instrument::Clap(r) = &c.instrument {
            inst.push(r.instance);
        }
    }
    for t in &d.project.tracks {
        for Insert::Clap(r) in &t.inserts {
            inst.push(r.instance);
        }
    }
    Ids {
        ch: d.project.channels.iter().map(|c| c.id).collect(),
        pa: d.project.patterns.iter().map(|c| c.id).collect(),
        tr: d.project.tracks.iter().map(|c| c.id).collect(),
        inst,
    }
}

fn all_notes(d: &Document, p: PatternId) -> Vec<NoteId> {
    d.project
        .pattern(p)
        .map(|pat| {
            pat.notes
                .iter()
                .flat_map(|c| c.notes.iter().map(|n| n.id))
                .collect()
        })
        .unwrap_or_default()
}

fn float(r: &mut Rng) -> f64 {
    match r.below(10) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1e9,
        3 => 1e9,
        _ => (r.below(2000) as f64 - 1000.0) / 8.0,
    }
}

fn gain(r: &mut Rng) -> f64 {
    if r.chance(90) {
        -(r.below(90) as f64)
    } else {
        float(r)
    }
}

/// Mostly valid edits against existing entities, with a share of garbage.
pub(crate) fn random_edit(r: &mut Rng, d: &Document) -> Edit {
    let ids = ids_of(d);
    let ch_id = |r: &mut Rng| {
        if r.chance(92) {
            r.pick(&ids.ch).unwrap_or(ChannelId(r.below(50) as u32))
        } else {
            ChannelId(r.below(60) as u32)
        }
    };
    let pa_id = |r: &mut Rng| {
        if r.chance(92) {
            r.pick(&ids.pa).unwrap_or(PatternId(r.below(50) as u32))
        } else {
            PatternId(r.below(60) as u32)
        }
    };
    let tr_id = |r: &mut Rng| {
        if r.chance(92) {
            r.pick(&ids.tr).unwrap_or(TrackId(r.below(50) as u32))
        } else {
            TrackId(r.below(60) as u32)
        }
    };
    let name = |r: &mut Rng| {
        if r.chance(95) {
            format!("n{}", r.below(1000))
        } else {
            String::new()
        }
    };
    match r.below(34) {
        0 => Edit::SetTempo {
            bpm: 20.0 + r.below(300) as f64 + if r.chance(5) { float(r) } else { 0.0 },
        },
        1 => Edit::SetTimeSigNum {
            num: r.below(19) as u8,
        },
        2 => Edit::SetMetronome {
            enabled: r.chance(50),
            gain_db: gain(r),
        },
        3 | 4 => Edit::AddChannel {
            name: name(r),
            instrument: if r.chance(80) {
                NewInstrument::Synth {
                    params: SynthParams::default(),
                }
            } else {
                NewInstrument::Clap {
                    plugin_id: format!("p.{}", r.below(5)),
                }
            },
            root_key: if r.chance(95) {
                r.below(128) as u8
            } else {
                200
            },
            track: tr_id(r),
        },
        5 => Edit::RemoveChannel { channel: ch_id(r) },
        6 => Edit::RenameChannel {
            channel: ch_id(r),
            name: name(r),
        },
        7 => Edit::SetChannelMix {
            channel: ch_id(r),
            value: match r.below(4) {
                0 => MixValue::VolumeDb(gain(r)),
                1 => MixValue::Pan(if r.chance(90) {
                    r.below(200) as f64 / 100.0 - 1.0
                } else {
                    float(r)
                }),
                2 => MixValue::Mute(r.chance(50)),
                _ => MixValue::Solo(r.chance(50)),
            },
        },
        8 => Edit::SetChannelTrack {
            channel: ch_id(r),
            track: tr_id(r),
        },
        9 | 10 => Edit::SetRootKey {
            channel: ch_id(r),
            key: r.below(130) as u8,
        },
        11 => Edit::SetSynthParam {
            channel: ch_id(r),
            param: SynthParam::ALL[r.below(17) as usize],
            value: float(r),
        },
        12 => Edit::SetSynthWave {
            channel: ch_id(r),
            osc: r.below(4) as u8,
            wave: [Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle][r.below(4) as usize],
        },
        13 | 14 => Edit::AddPattern {
            name: name(r),
            length_steps: r.below(70) as u8,
        },
        15 => Edit::RemovePattern { pattern: pa_id(r) },
        16 => Edit::RenamePattern {
            pattern: pa_id(r),
            name: name(r),
        },
        17 => Edit::SetPatternLength {
            pattern: pa_id(r),
            length_steps: r.below(70) as u8,
        },
        18 => Edit::SetStepTicks {
            pattern: pa_id(r),
            step_ticks: [60u32, 120, 240, 480, 960, 3840, 0, 5000][r.below(8) as usize],
        },
        19..=22 => Edit::SetStep {
            pattern: pa_id(r),
            channel: ch_id(r),
            step: r.below(70) as u8,
            on: r.chance(70),
            vel: if r.chance(60) {
                None
            } else {
                Some(r.below(130) as u8)
            },
        },
        23 | 24 => {
            let n = r.below(6) as usize;
            Edit::AddNotes {
                pattern: pa_id(r),
                channel: ch_id(r),
                notes: (0..n)
                    .map(|_| NewNote {
                        start: r.below(4200) as u32,
                        len: if r.chance(95) {
                            1 + r.below(2000) as u32
                        } else {
                            0
                        },
                        key: r.below(130) as u8,
                        vel: r.below(130) as u8,
                    })
                    .collect(),
            }
        }
        25..=29 => {
            let p = pa_id(r);
            let have = all_notes(d, p);
            let n = r.below(4) as usize;
            let mut nids: Vec<NoteId> = (0..n).filter_map(|_| r.pick(&have)).collect();
            if r.chance(3) {
                nids.push(NoteId(r.below(100000) as u32));
            }
            match r.below(5) {
                0 => Edit::RemoveNotes {
                    pattern: p,
                    notes: nids,
                },
                1 => Edit::MoveNotes {
                    pattern: p,
                    notes: nids,
                    dt: r.below(2000) as i64 - 1000,
                    dkey: r.below(30) as i16 - 15,
                },
                2 => Edit::ResizeNotes {
                    pattern: p,
                    notes: nids,
                    dlen: r.below(800) as i64 - 400,
                },
                3 => Edit::SetNoteVelocity {
                    pattern: p,
                    notes: nids,
                    vel: r.below(130) as u8,
                },
                _ => Edit::MoveNotes {
                    pattern: p,
                    notes: nids,
                    dt: if r.chance(50) { i64::MAX } else { i64::MIN },
                    dkey: i16::MAX,
                },
            }
        }
        30 => Edit::AddTrack { name: name(r) },
        31 => {
            if r.chance(30) {
                Edit::RemoveTrack { track: tr_id(r) }
            } else {
                Edit::RenameTrack {
                    track: tr_id(r),
                    name: name(r),
                }
            }
        }
        32 => Edit::SetTrackMix {
            track: tr_id(r),
            value: MixValue::VolumeDb(gain(r)),
        },
        _ => match r.below(5) {
            0 => Edit::AddInsert {
                track: tr_id(r),
                index: r.below(10) as u8,
                plugin_id: format!("i.{}", r.below(4)),
            },
            1 => {
                let t = tr_id(r);
                let inst = d
                    .project
                    .track(t)
                    .and_then(|tt| tt.inserts.first().map(|Insert::Clap(c)| c.instance))
                    .unwrap_or(InstanceId(1));
                Edit::RemoveInsert {
                    track: t,
                    instance: inst,
                }
            }
            2 => Edit::SetPluginParam {
                instance: r.pick(&ids.inst).unwrap_or(InstanceId(1)),
                param_id: r.below(8) as u32,
                value: float(r),
            },
            3 => {
                let i = r.pick(&ids.inst).unwrap_or(InstanceId(1));
                Edit::CommitPluginState {
                    instance: i,
                    state_file: state_file_name(i, 1 + r.below(5) as u32),
                }
            }
            _ => Edit::CommitPluginState {
                instance: r.pick(&ids.inst).unwrap_or(InstanceId(1)),
                state_file: "bad/name".into(),
            },
        },
    }
}

#[test]
fn random_edit_sequences_never_produce_an_invalid_document() {
    for seed in 1..=40u64 {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xA5A5_1234_5678_9ABC));
        let mut d = Document::new();
        let mut seen: HashSet<u32> = HashSet::new();
        let (mut accepted, mut rejected) = (0u32, 0u32);
        for step in 0..400 {
            let e = random_edit(&mut r, &d);
            match apply(&d, &e) {
                Ok((nd, created)) => {
                    accepted += 1;
                    validate(&nd.project).unwrap_or_else(|err| {
                        panic!("seed {seed} step {step}: invalid after {e:?}: {err}")
                    });
                    protocol::format::emit(&nd.project, nd.next_id)
                        .unwrap_or_else(|err| panic!("seed {seed}: {e:?} does not save: {err}"));
                    for id in created {
                        assert!(seen.insert(id), "seed {seed}: id {id} repeated");
                        assert!(id < nd.next_id);
                    }
                    assert!(nd.next_id >= d.next_id);
                    assert_eq!(nd.revision, d.revision + 1);
                    d = nd;
                }
                Err(_) => rejected += 1,
            }
            assert!(d.next_id > d.project.max_id());
        }
        assert!(accepted > 100, "seed {seed}: only {accepted} accepted");
        assert!(rejected > 5, "seed {seed}: only {rejected} rejected");
    }
}

#[test]
fn random_batches_are_atomic_and_valid() {
    for seed in 1..=20u64 {
        let mut r = Rng(0xD1B5_4A32_D192_ED03 ^ seed.wrapping_mul(0x1234_5678_9ABC_DEF1));
        let mut d = Document::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for _ in 0..60 {
            let n = 1 + r.below(12) as usize;
            let mut probe = d.clone();
            let mut edits = Vec::new();
            for _ in 0..n {
                let e = random_edit(&mut r, &probe);
                if let Ok((nd, _)) = apply(&probe, &e) {
                    probe = nd;
                }
                edits.push(e);
            }
            let before = (d.project.clone(), d.next_id, d.revision);
            match apply_batch(&d, &edits) {
                Ok((nd, created)) => {
                    validate(&nd.project).unwrap();
                    assert_eq!(nd.revision, d.revision + 1);
                    for id in created {
                        assert!(seen.insert(id));
                    }
                    d = nd;
                }
                Err(_) => {
                    assert!(Arc::ptr_eq(&before.0, &d.project));
                    assert_eq!((before.1, before.2), (d.next_id, d.revision));
                }
            }
        }
    }
}

#[test]
fn max_size_batch_applies() {
    let (d, c, p) = base();
    let edits: Vec<Edit> = (0..MAX_EDITS_PER_REQUEST)
        .map(|i| Edit::AddNotes {
            pattern: p,
            channel: c,
            notes: vec![NewNote {
                start: (i % 3000) as u32,
                len: 10,
                key: (i % 128) as u8,
                vel: 100,
            }],
        })
        .collect();
    let (d2, created) = apply_batch(&d, &edits).unwrap();
    assert_eq!(created.len(), MAX_EDITS_PER_REQUEST);
    assert_eq!(d2.project.note_count(), MAX_EDITS_PER_REQUEST);
}

#[test]
fn failing_edit_index_is_reported() {
    let (d, c, _) = base();
    let edits = [
        Edit::SetTempo { bpm: 90.0 },
        Edit::RenameChannel {
            channel: c,
            name: "ok".into(),
        },
        Edit::SetTempo { bpm: 5000.0 },
    ];
    let (idx, _) = apply_batch_indexed(&d, &edits).unwrap_err();
    assert_eq!(idx, Some(2));
    let bad = [
        Edit::SetTempo { bpm: 90.0 },
        Edit::RemoveChannel {
            channel: ChannelId(4242),
        },
    ];
    assert_eq!(apply_batch_indexed(&d, &bad).unwrap_err().0, Some(1));
}
