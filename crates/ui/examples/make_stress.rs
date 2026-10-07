// SPDX-License-Identifier: GPL-3.0-or-later
//! Writes a project with 2,000 notes on one channel, zoomed so all of them
//! are visible, for the drawing numbers (`LIBREDAW_DEBUG`):
//! `cargo run -p libredaw-ui --example make_stress -- /tmp/Stress.ldaw`

use protocol::edit::{Edit, NewInstrument, NewNote};
use protocol::ids::{ChannelId, PatternId, TrackId};
use protocol::model::SynthParams;
use ui::bundle;
use ui::document::{Document, apply_batch};
use ui::persist::ViewState;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: make_stress <bundle dir>");
        std::process::exit(2);
    };
    let (d, ids) = apply_batch(
        &Document::new(),
        &[
            Edit::AddPattern {
                name: "Stress".into(),
                length_steps: 64,
            },
            Edit::AddChannel {
                name: "Notes".into(),
                instrument: NewInstrument::Synth {
                    params: SynthParams::default(),
                },
                root_key: 60,
                track: TrackId::MASTER,
            },
        ],
    )
    .expect("setup");
    let (pattern, channel) = (PatternId(ids[0]), ChannelId(ids[1]));
    // 64 steps of 240 ticks: 15,360 ticks. 2,000 short notes spread over
    // 100 keys and the whole length.
    let notes: Vec<NewNote> = (0..2000u32)
        .map(|i| NewNote {
            start: (i * 7) % 15_000,
            len: 120,
            key: 14 + ((i * 37) % 100) as u8,
            vel: 60 + (i % 60) as u8,
        })
        .collect();
    let (d, _) = apply_batch(
        &d,
        &[Edit::AddNotes {
            pattern,
            channel,
            notes,
        }],
    )
    .expect("notes");
    bundle::save(std::path::Path::new(&path), &d).expect("save");
    // All 2,000 notes in view at 1920 x 1080.
    let view = ViewState {
        px_per_tick: 0.115,
        row_h: 6.0,
        scroll_x: 0.0,
        scroll_y: 0.0,
        focus: "notes".into(),
        ..ViewState::default()
    };
    view.write(std::path::Path::new(&path)).expect("view");
    println!("wrote {path}");
}
