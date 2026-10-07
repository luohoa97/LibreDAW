// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio file decoding for samples (SPEC 15.3): WAV (PCM, and Ogg Vorbis
//! inside WAV as FL Studio stores it), FLAC, Ogg Vorbis, MP3 and WavPack.
//!
//! Everything decodes to interleaved `f32` at the file's own rate. The
//! functions here block and allocate: call them from a loader thread, never
//! from the audio thread. Files are untrusted: input and output sizes are
//! limited, and a damaged file gives an [`Error`], never a panic.

use std::io::Cursor;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded audio: interleaved `f32` frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub rate: u32,
    pub channels: u16,
    pub data: Vec<f32>,
    /// Set when the stream ended in damage after some audio was decoded: the
    /// audio up to that point is returned and this says what went wrong.
    pub partial: Option<Error>,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.data.len() / usize::from(self.channels.max(1))
    }
}

/// Why a file did not decode. Cloneable and printable, so a loader can
/// queue it as a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    /// A format or codec variant we do not read.
    Unsupported(String),
    /// The file is damaged and nothing could be decoded.
    Corrupt(String),
    /// The file is longer than the limits allow.
    TooLarge,
    /// Decoded fine but holds no audio frames.
    Empty,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(m) => write!(f, "cannot read the file: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported audio format: {m}"),
            Error::Corrupt(m) => write!(f, "damaged audio file: {m}"),
            Error::TooLarge => f.write_str("audio file is too large"),
            Error::Empty => f.write_str("audio file has no audio"),
        }
    }
}

impl std::error::Error for Error {}

/// Size limits for untrusted files.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest input file.
    pub max_input_bytes: u64,
    /// Largest decoded size, in `f32` values (all channels).
    pub max_samples: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_input_bytes: 1 << 30,
            // 10 minutes of 48 kHz stereo is 57.6 M values; allow 4x that.
            max_samples: 256 << 20,
        }
    }
}

/// File extensions this crate can decode (lower case, without the dot).
pub const EXTENSIONS: [&str; 7] = ["wav", "wave", "flac", "ogg", "oga", "mp3", "wv"];

pub fn is_supported_ext(ext: &str) -> bool {
    EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

pub fn decode_file(path: &Path) -> Result<Audio, Error> {
    decode_file_with(path, Limits::default())
}

pub fn decode_file_with(path: &Path, limits: Limits) -> Result<Audio, Error> {
    let len = std::fs::metadata(path)
        .map_err(|e| Error::Io(e.to_string()))?
        .len();
    if len > limits.max_input_bytes {
        return Err(Error::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?;
    decode_bytes_with(&bytes, path.extension().and_then(|e| e.to_str()), limits)
}

pub fn decode_bytes(bytes: &[u8], ext_hint: Option<&str>) -> Result<Audio, Error> {
    decode_bytes_with(bytes, ext_hint, Limits::default())
}

/// Decodes by content; `ext_hint` only helps tell MP3 from noise.
pub fn decode_bytes_with(
    bytes: &[u8],
    ext_hint: Option<&str>,
    limits: Limits,
) -> Result<Audio, Error> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(Error::TooLarge);
    }
    if bytes.starts_with(b"wvpk") {
        return decode_wavpack(bytes, limits);
    }
    if bytes.len() >= 12
        && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RF64")
        && &bytes[8..12] == b"WAVE"
    {
        match riff_vorbis(bytes)? {
            Some(ogg) => return decode_symphonia(ogg, Some("ogg"), limits),
            None => return decode_symphonia(bytes, Some("wav"), limits),
        }
    }
    decode_symphonia(bytes, ext_hint, limits)
}

/// For WAV files with an Ogg Vorbis payload (format tags 0x674E to 0x6751):
/// the Ogg stream inside the data chunk. `None` for any other WAV.
///
/// FL Studio writes tag 0x674F (headers and audio in the data chunk) and
/// 0x6750 (the same, and the headers also repeated in the `fmt ` chunk).
/// Both carry a complete Ogg stream in `data`, which is all that is read.
fn riff_vorbis(b: &[u8]) -> Result<Option<&[u8]>, Error> {
    let mut pos = 12usize;
    let mut tag = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 2 <= b.len() {
            tag = Some(u16::from_le_bytes([b[body], b[body + 1]]));
        } else if id == b"data" {
            let Some(t) = tag.filter(|t| (0x674E..=0x6751).contains(t)) else {
                return Ok(None);
            };
            let end = body.saturating_add(size).min(b.len());
            let data = &b[body..end];
            if data.starts_with(b"OggS") {
                return Ok(Some(data));
            }
            return Err(Error::Unsupported(format!(
                "Vorbis in WAV, format tag {t:#06x}, without Ogg pages in the data chunk"
            )));
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Ok(None)
}

fn map_sym(e: SymError) -> Error {
    match e {
        SymError::IoError(e) => Error::Io(e.to_string()),
        SymError::Unsupported(m) => Error::Unsupported(m.to_string()),
        other => Error::Corrupt(other.to_string()),
    }
}

fn decode_symphonia(bytes: &[u8], ext: Option<&str>, limits: Limits) -> Result<Audio, Error> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext {
        hint.with_extension(e);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| match e {
            SymError::Unsupported(_) => Error::Unsupported("unknown audio format".into()),
            other => map_sym(other),
        })?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| Error::Unsupported("no audio track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| Error::Unsupported("no audio parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(map_sym)?;
    let track_id = track.id;

    let mut data: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let (mut rate, mut channels) = (0u32, 0u16);
    let mut partial = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // FL writes some Ogg streams without their last page: a clean end.
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                partial = Some(map_sym(e));
                break;
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                if rate == 0 {
                    rate = spec.rate();
                    channels = u16::try_from(spec.channels().count()).unwrap_or(0);
                }
                scratch.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut scratch);
                if data.len() + scratch.len() > limits.max_samples {
                    return Err(Error::TooLarge);
                }
                data.extend_from_slice(&scratch);
            }
            // A bad packet is skipped; the rest of the file may be fine.
            Err(SymError::DecodeError(m)) => {
                partial = partial.or(Some(Error::Corrupt(m.to_string())))
            }
            Err(e) => {
                partial = Some(map_sym(e));
                break;
            }
        }
    }
    finish(rate, channels, data, partial)
}

fn finish(
    rate: u32,
    channels: u16,
    data: Vec<f32>,
    partial: Option<Error>,
) -> Result<Audio, Error> {
    if rate == 0 || channels == 0 || data.is_empty() {
        return Err(partial.unwrap_or(Error::Empty));
    }
    Ok(Audio {
        rate,
        channels,
        data,
        partial,
    })
}

/// The leading run of WavPack blocks: files may carry an APEv2 or ID3v1 tag
/// after the last block, which the block decoder would reject.
fn wv_blocks(b: &[u8]) -> &[u8] {
    let mut pos = 0usize;
    while pos + 32 <= b.len() && &b[pos..pos + 4] == b"wvpk" {
        let ck = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        match pos.checked_add(ck).and_then(|n| n.checked_add(8)) {
            Some(next) if next <= b.len() => pos = next,
            _ => break,
        }
    }
    if pos == 0 { b } else { &b[..pos] }
}

fn decode_wavpack(bytes: &[u8], limits: Limits) -> Result<Audio, Error> {
    let bytes = wv_blocks(bytes);
    let s = wavicle::decode_stream(bytes).map_err(|e| match e {
        wavicle::Error::NotYetImplemented(m) => Error::Unsupported(format!("WavPack: {m}")),
        other => Error::Corrupt(format!("WavPack: {other}")),
    })?;
    if s.samples.len() > limits.max_samples {
        return Err(Error::TooLarge);
    }
    let data: Vec<f32> = if s.is_float {
        s.samples
            .iter()
            .map(|&v| f32::from_bits(v as u32))
            .collect()
    } else {
        let scale = 1.0 / (2f64.powi(s.bits_per_sample.clamp(8, 32) as i32 - 1)) as f32;
        s.samples.iter().map(|&v| v as f32 * scale).collect()
    };
    finish(
        s.sample_rate,
        u16::try_from(s.channels).unwrap_or(0),
        data,
        None,
    )
}
