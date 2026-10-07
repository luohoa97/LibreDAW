// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-written WAV writer (SPEC 8): 16 and 24 bit PCM with TPDF dither,
//! or 32-bit float.

use protocol::control::WavFormat;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// xorshift64*: a fixed-seed generator, so the same render dithers the same
/// way every time.
struct Rng(u64);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// Uniform in [-0.5, 0.5).
    fn uniform(&mut self) -> f32 {
        self.next_u32() as f32 / 4_294_967_296.0 - 0.5
    }
}

fn quantize(x: f32, bits: u32, rng: &mut Rng) -> i32 {
    let scale = (1u32 << (bits - 1)) as f32;
    // TPDF: the sum of two uniform variables, +-1 LSB peak.
    let dither = rng.uniform() + rng.uniform();
    let v = (x * scale + dither).round();
    v.clamp(-scale, scale - 1.0) as i32
}

/// Writes `frames` (stereo) as a RIFF/WAVE stream.
pub fn write_wav_to<W: Write>(
    w: &mut W,
    frames: &[[f32; 2]],
    rate: u32,
    fmt: WavFormat,
) -> io::Result<()> {
    let (tag, bits): (u16, u16) = match fmt {
        WavFormat::Pcm16 => (1, 16),
        WavFormat::Pcm24 => (1, 24),
        WavFormat::Float32 => (3, 32),
    };
    let channels: u16 = 2;
    let block_align = channels * bits / 8;
    let data_len = u32::try_from(frames.len() as u64 * block_align as u64)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "render too long for WAV"))?;
    let float = tag == 3;
    // fmt is 16 bytes for PCM and 18 for float; float also needs a fact chunk.
    let fmt_len: u32 = if float { 18 } else { 16 };
    let fact_len: u32 = if float { 12 } else { 0 };
    let riff_len = 4 + (8 + fmt_len) + fact_len + (8 + data_len + (data_len & 1));

    w.write_all(b"RIFF")?;
    w.write_all(&riff_len.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&fmt_len.to_le_bytes())?;
    w.write_all(&tag.to_le_bytes())?;
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * block_align as u32).to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    if float {
        w.write_all(&0u16.to_le_bytes())?; // cbSize
        w.write_all(b"fact")?;
        w.write_all(&4u32.to_le_bytes())?;
        w.write_all(&(frames.len() as u32).to_le_bytes())?;
    }
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for f in frames {
        for &x in f {
            match fmt {
                WavFormat::Pcm16 => {
                    w.write_all(&(quantize(x, 16, &mut rng) as i16).to_le_bytes())?
                }
                WavFormat::Pcm24 => {
                    let v = quantize(x, 24, &mut rng);
                    w.write_all(&v.to_le_bytes()[..3])?
                }
                WavFormat::Float32 => w.write_all(&x.to_le_bytes())?,
            }
        }
    }
    if data_len & 1 == 1 {
        w.write_all(&[0])?;
    }
    Ok(())
}

pub fn write_wav(path: &Path, frames: &[[f32; 2]], rate: u32, fmt: WavFormat) -> io::Result<()> {
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    write_wav_to(&mut w, frames, rate, fmt)?;
    w.flush()
}
