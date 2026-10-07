// SPDX-License-Identifier: GPL-3.0-or-later
//! Basic Pitch inference: windowing, the ONNX model, and unwrapping the
//! overlapping outputs (port of `inference.py`).

use crate::{Error, Result};
use std::sync::{Arc, OnceLock};
use tract_onnx::prelude::*;

const MODEL_BYTES: &[u8] = include_bytes!("../model/nmp.onnx");

pub const FFT_HOP: usize = 256;
pub const SAMPLE_RATE: usize = 22050;
pub const N_PITCHES: usize = 88;
/// Input window length in samples (2 s minus one hop).
const WINDOW: usize = SAMPLE_RATE * 2 - FFT_HOP;
const OVERLAP_FRAMES: usize = 30;
const OVERLAP: usize = OVERLAP_FRAMES * FFT_HOP;
const HOP: usize = WINDOW - OVERLAP;
/// Output frames per window.
const FRAMES_PER_WINDOW: usize = 2 * (SAMPLE_RATE / FFT_HOP);

type Plan = TypedRunnableModel<TypedModel>;

/// Model activations, row-major `[frame][pitch]` with 88 pitches from MIDI 21.
pub struct Activations {
    pub frames: Vec<f32>,
    pub onsets: Vec<f32>,
    pub n_frames: usize,
}

fn plan() -> Result<Arc<Plan>> {
    static PLAN: OnceLock<std::result::Result<Arc<Plan>, String>> = OnceLock::new();
    PLAN.get_or_init(|| load().map(Arc::new).map_err(|e| e.to_string()))
        .clone()
        .map_err(Error::Model)
}

fn load() -> TractResult<Plan> {
    let mut model = tract_onnx::onnx().model_for_read(&mut &MODEL_BYTES[..])?;
    model.set_output_names([
        "StatefulPartitionedCall:1", // note
        "StatefulPartitionedCall:2", // onset
        "StatefulPartitionedCall:0", // contour
    ])?;
    model
        .with_input_fact(0, f32::fact([1, WINDOW, 1]).into())?
        .into_optimized()?
        .into_runnable()
}

fn merr(e: impl std::fmt::Display) -> Error {
    Error::Model(e.to_string())
}

/// Runs the model over `audio` (22050 Hz mono).
pub fn run(audio: &[f32]) -> Result<Activations> {
    let plan = plan()?;
    let original_len = audio.len();
    let mut padded = vec![0.0f32; OVERLAP / 2];
    padded.extend_from_slice(audio);

    let keep = FRAMES_PER_WINDOW - OVERLAP_FRAMES; // frames kept per window
    let mut frames: Vec<f32> = Vec::new();
    let mut onsets: Vec<f32> = Vec::new();
    for start in (0..padded.len()).step_by(HOP) {
        let mut window = vec![0.0f32; WINDOW];
        let end = (start + WINDOW).min(padded.len());
        window[..end - start].copy_from_slice(&padded[start..end]);
        let input = Tensor::from_shape(&[1, WINDOW, 1], &window).map_err(merr)?;
        let out = plan.run(tvec!(input.into())).map_err(merr)?;
        for (idx, dst) in [(0usize, &mut frames), (1, &mut onsets)] {
            let view = out[idx].to_array_view::<f32>().map_err(merr)?;
            let v: Vec<f32> = view.iter().copied().collect();
            // Output is [1, FRAMES_PER_WINDOW, N_PITCHES]; drop half the overlap each side.
            if v.len() != FRAMES_PER_WINDOW * N_PITCHES {
                return Err(Error::Model(format!("unexpected output size {}", v.len())));
            }
            let a = OVERLAP_FRAMES / 2 * N_PITCHES;
            dst.extend_from_slice(&v[a..a + keep * N_PITCHES]);
        }
    }
    let expected = (original_len as f64 / HOP as f64 * keep as f64) as usize;
    let n_frames = (frames.len() / N_PITCHES).min(expected);
    frames.truncate(n_frames * N_PITCHES);
    onsets.truncate(n_frames * N_PITCHES);
    Ok(Activations {
        frames,
        onsets,
        n_frames,
    })
}

/// Time in seconds of an unwrapped output frame. Each window contributes
/// `keep` frames but advances the input by `HOP` samples, which is not a
/// whole number of frames, so the time is computed from the window start.
pub fn frame_time(frame: usize) -> f64 {
    let keep = FRAMES_PER_WINDOW - OVERLAP_FRAMES;
    let (w, j) = (frame / keep, frame % keep);
    (w * HOP + j * FFT_HOP) as f64 / SAMPLE_RATE as f64
}
