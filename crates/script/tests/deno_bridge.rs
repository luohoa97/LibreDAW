// SPDX-License-Identifier: GPL-3.0-or-later
//! The Deno bridge against real `deno` and real scripts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use protocol::control::{ControlError, Outcome, ReplyBody, RequestBody};
use protocol::edit::NewNote;
use protocol::edit::{Applied, Edit, MixValue};
use protocol::ids::{ChannelId, NoteId, PatternId, TrackId};
use protocol::model::Project;
use script::deno::{KillReason, ScriptEnd, ScriptError, ScriptOptions, check_deno, run_script};

fn script_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/scripts")
        .join(name)
}

type Log = Vec<(RequestBody, Option<u64>)>;

fn project_reply(revision: u64) -> Outcome {
    Outcome::Ok {
        body: ReplyBody::Project {
            revision,
            project: Arc::new(Project::empty()),
        },
    }
}

#[test]
fn deno_is_found_and_new_enough() {
    let v = check_deno(&PathBuf::from("deno")).expect("deno on PATH");
    assert!(v.starts_with(|c: char| c.is_ascii_digit()), "{v}");
    assert!(matches!(
        check_deno(&PathBuf::from("/nonexistent/deno")),
        Err(ScriptError::DenoMissing(_))
    ));
}

#[test]
fn project_get_and_edit_round_trip() {
    let mut log: Log = Vec::new();
    let report = run_script(
        &script_path("edit_roundtrip.ts"),
        &ScriptOptions::default(),
        &mut |body, base| {
            log.push((body.clone(), base));
            match body {
                RequestBody::ProjectGet => project_reply(5),
                RequestBody::Edit { edits } if edits.len() == 8 => Outcome::Ok {
                    body: ReplyBody::Applied(Applied {
                        revision: 8,
                        created: vec![11, 12],
                    }),
                },
                RequestBody::Edit { .. } => Outcome::Ok {
                    body: ReplyBody::Applied(Applied {
                        revision: 9,
                        created: vec![],
                    }),
                },
                other => panic!("unexpected {other:?}"),
            }
        },
    )
    .unwrap();
    assert!(report.succeeded(), "{:?}\n{}", report.end, report.stderr);
    assert!(
        report.stderr.contains("this goes to stderr"),
        "console.log is on stderr: {}",
        report.stderr
    );
    assert_eq!(report.requests, 3);
    assert_eq!(log.len(), 3);
    assert_eq!(log[0], (RequestBody::ProjectGet, None));
    // First batch: checked against the revision project.get returned.
    let (RequestBody::Edit { edits }, base) = &log[1] else {
        panic!()
    };
    assert_eq!(*base, Some(5));
    assert_eq!(
        edits,
        &vec![
            Edit::SetTempo { bpm: 140.0 },
            Edit::AddNotes {
                pattern: PatternId(3),
                channel: ChannelId(4),
                notes: vec![NewNote {
                    start: 0,
                    len: 240,
                    key: 36,
                    vel: 100
                }],
            },
            Edit::SetStep {
                pattern: PatternId(3),
                channel: ChannelId(4),
                step: 2,
                on: true,
                vel: None,
            },
            Edit::RemoveNotes {
                pattern: PatternId(3),
                notes: vec![NoteId(9)],
            },
            Edit::SetTrackMix {
                track: TrackId::MASTER,
                value: MixValue::VolumeDb(-3.0),
            },
            Edit::SetChannelMix {
                channel: ChannelId(4),
                value: MixValue::Pan(0.5),
            },
            Edit::SetChannelMix {
                channel: ChannelId(4),
                value: MixValue::Mute(true),
            },
            Edit::SetTrackMix {
                track: TrackId(1),
                value: MixValue::Solo(false),
            },
        ]
    );
    // Second batch: checked against the revision the first one returned.
    assert_eq!(log[2].1, Some(8));
}

#[test]
fn daw_errors_reach_the_script_as_typed_errors() {
    let mut n = 0;
    let report = run_script(
        &script_path("errors.ts"),
        &ScriptOptions::default(),
        &mut |body, _| {
            n += 1;
            match (n, body) {
                (1, RequestBody::ProjectGet) => project_reply(5),
                (2, RequestBody::Edit { .. }) => Outcome::Err {
                    error: ControlError::Stale { current: 9 },
                },
                (3, RequestBody::Edit { .. }) => Outcome::Err {
                    error: ControlError::NeedsUserApproval,
                },
                (n, other) => panic!("request {n}: {other:?}"),
            }
        },
    )
    .unwrap();
    assert!(report.succeeded(), "{:?}\n{}", report.end, report.stderr);
    // project.get, stale edit, approval edit. The malformed one never
    // reaches the handler.
    assert_eq!(report.requests, 4);
}

#[test]
fn script_has_no_permissions_except_reading_itself() {
    let report = run_script(
        &script_path("sandbox.ts"),
        &ScriptOptions::default(),
        &mut |body, _| panic!("unexpected {body:?}"),
    )
    .unwrap();
    assert!(report.succeeded(), "{:?}\n{}", report.end, report.stderr);
}

#[test]
fn script_that_never_answers_init_is_killed() {
    let t = Instant::now();
    let report = run_script(
        &script_path("hang_before_ready.ts"),
        &ScriptOptions::default(),
        &mut |body, _| panic!("unexpected {body:?}"),
    )
    .unwrap();
    assert_eq!(report.end, ScriptEnd::Killed(KillReason::NoReady));
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(4),
        "{took:?}"
    );
}

#[test]
fn script_that_runs_too_long_is_killed() {
    let opts = ScriptOptions {
        max_runtime: Duration::from_secs(3),
        ..ScriptOptions::default()
    };
    let t = Instant::now();
    let report = run_script(
        &script_path("hang_after_ready.ts"),
        &opts,
        &mut |body, _| match body {
            RequestBody::ProjectGet => project_reply(1),
            other => panic!("unexpected {other:?}"),
        },
    )
    .unwrap();
    assert_eq!(report.end, ScriptEnd::Killed(KillReason::MaxRuntime));
    assert!(t.elapsed() < Duration::from_secs(6));
}

#[test]
fn exit_code_and_stderr_are_reported() {
    let report = run_script(
        &script_path("exit_code.ts"),
        &ScriptOptions::default(),
        &mut |body, _| panic!("unexpected {body:?}"),
    )
    .unwrap();
    assert_eq!(report.end, ScriptEnd::Exited(Some(3)));
    assert!(report.stderr.contains("about to fail"));
}

#[test]
fn missing_deno_and_missing_script_are_errors() {
    let opts = ScriptOptions {
        deno: PathBuf::from("/nonexistent/deno"),
        ..ScriptOptions::default()
    };
    let r = run_script(
        &script_path("exit_code.ts"),
        &opts,
        &mut |_, _| unreachable!(),
    );
    assert!(matches!(r, Err(ScriptError::DenoMissing(_))));
    let r = run_script(
        &script_path("does_not_exist.ts"),
        &ScriptOptions::default(),
        &mut |_, _| unreachable!(),
    );
    assert!(matches!(r, Err(ScriptError::BadScriptPath(_))));
}
