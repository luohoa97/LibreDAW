// SPDX-License-Identifier: GPL-3.0-or-later
//! WAV reader (SPEC 15.1, 17.2): PCM 8/16/24/32-bit integer and 32-bit
//! float, mono or stereo. Our own code, written for untrusted files: every
//! size field is checked against the bytes that are really there, nothing is
//! allocated from a size the file only claims, and nothing panics.

/// Decoded audio: interleaved `f32` frames at the file's own rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub rate: u32,
    pub channels: u8,
    pub data: Vec<f32>,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels.max(1) as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE file.
    NotWav,
    /// A chunk or the header ends before its declared size.
    Truncated,
    /// No `fmt ` chunk before the data, or no `data` chunk.
    Missing(&'static str),
    /// A field has a value we reject (the text names it).
    Invalid(&'static str),
    /// A valid format we do not read (compressed, 64-bit, more than 2 channels).
    Unsupported(String),
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::NotWav => f.write_str("not a WAV file"),
            WavError::Truncated => f.write_str("WAV file is truncated"),
            WavError::Missing(w) => write!(f, "WAV file has no {w} chunk"),
            WavError::Invalid(w) => write!(f, "WAV file has an invalid {w}"),
            WavError::Unsupported(w) => write!(f, "unsupported WAV format: {w}"),
        }
    }
}

impl std::error::Error for WavError {}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

#[derive(Clone, Copy)]
struct Fmt {
    float: bool,
    channels: u8,
    rate: u32,
    bits: u16,
}

fn parse_fmt(c: &[u8]) -> Result<Fmt, WavError> {
    if c.len() < 16 {
        return Err(WavError::Truncated);
    }
    let mut tag = u16_at(c, 0).ok_or(WavError::Truncated)?;
    let channels = u16_at(c, 2).ok_or(WavError::Truncated)?;
    let rate = u32_at(c, 4).ok_or(WavError::Truncated)?;
    let align = u16_at(c, 12).ok_or(WavError::Truncated)?;
    let bits = u16_at(c, 14).ok_or(WavError::Truncated)?;
    if tag == 0xFFFE {
        // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts at byte 24.
        if c.len() < 26 {
            return Err(WavError::Truncated);
        }
        tag = u16_at(c, 24).ok_or(WavError::Truncated)?;
    }
    let float = match tag {
        1 => false,
        3 => true,
        t => return Err(WavError::Unsupported(format!("format tag {t}"))),
    };
    match channels {
        1 | 2 => {}
        0 => return Err(WavError::Invalid("channel count")),
        n => return Err(WavError::Unsupported(format!("{n} channels"))),
    }
    if !(1000..=768_000).contains(&rate) {
        return Err(WavError::Invalid("sample rate"));
    }
    match (float, bits) {
        (false, 8 | 16 | 24 | 32) | (true, 32) => {}
        (_, b) => return Err(WavError::Unsupported(format!("{b}-bit samples"))),
    }
    if align as u32 != channels as u32 * (bits as u32 / 8) {
        return Err(WavError::Invalid("block alignment"));
    }
    Ok(Fmt {
        float,
        channels: channels as u8,
        rate,
        bits,
    })
}

/// Decodes a whole WAV file held in memory.
pub fn decode_wav(b: &[u8]) -> Result<Decoded, WavError> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(WavError::NotWav);
    }
    let mut at = 12usize;
    let mut fmt: Option<Fmt> = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let size = u32_at(b, at + 4).ok_or(WavError::Truncated)? as usize;
        let body = at + 8;
        let avail = b.len() - body;
        if id == b"data" {
            let f = fmt.ok_or(WavError::Missing("fmt"))?;
            // A data chunk that claims more than the file holds (a writer
            // that never went back to patch the size) plays what is there.
            let len = size.min(avail);
            return decode_data(f, &b[body..body + len]);
        }
        if size > avail {
            return Err(WavError::Truncated);
        }
        if id == b"fmt " {
            fmt = Some(parse_fmt(&b[body..body + size])?);
        }
        // Chunks are padded to an even size.
        at = body + size + (size & 1);
    }
    Err(WavError::Missing(if fmt.is_none() {
        "fmt"
    } else {
        "data"
    }))
}

fn decode_data(f: Fmt, d: &[u8]) -> Result<Decoded, WavError> {
    let bytes = f.bits as usize / 8;
    let frame = bytes * f.channels as usize;
    let frames = d.len() / frame;
    if frames == 0 {
        return Err(WavError::Invalid("data size"));
    }
    let n = frames * f.channels as usize;
    let mut out = Vec::with_capacity(n);
    for s in d[..n * bytes].chunks_exact(bytes) {
        out.push(match (f.float, f.bits) {
            (false, 8) => (s[0] as f32 - 128.0) / 128.0,
            (false, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            (false, 24) => {
                let v = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
                v as f32 / 8_388_608.0
            }
            (false, _) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (true, _) => {
                let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                if v.is_finite() { v } else { 0.0 }
            }
        });
    }
    Ok(Decoded {
        rate: f.rate,
        channels: f.channels,
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a WAV file with the given format and raw data.
    pub fn wav_bytes(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * align as u32).to_le_bytes());
        v.extend_from_slice(&align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn reads_every_supported_format() {
        let d8 = wav_bytes(1, 1, 44100, 8, &[0, 128, 255, 64]);
        let r = decode_wav(&d8).unwrap();
        assert_eq!((r.rate, r.channels, r.frames()), (44100, 1, 4));
        assert_eq!(r.data[0], -1.0);
        assert_eq!(r.data[1], 0.0);
        assert!((r.data[2] - 127.0 / 128.0).abs() < 1e-6);
        assert_eq!(r.data[3], -0.5);

        let mut d16 = Vec::new();
        for v in [0i16, 16384, -32768, 32767] {
            d16.extend_from_slice(&v.to_le_bytes());
        }
        let r = decode_wav(&wav_bytes(1, 2, 48000, 16, &d16)).unwrap();
        assert_eq!((r.channels, r.frames()), (2, 2));
        assert_eq!(r.data, vec![0.0, 0.5, -1.0, 32767.0 / 32768.0]);

        let mut d24 = Vec::new();
        for v in [0i32, 4_194_304, -8_388_608] {
            d24.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        let r = decode_wav(&wav_bytes(1, 1, 48000, 24, &d24)).unwrap();
        assert_eq!(r.data, vec![0.0, 0.5, -1.0]);

        let mut d32 = Vec::new();
        for v in [0i32, 1 << 30, i32::MIN] {
            d32.extend_from_slice(&v.to_le_bytes());
        }
        let r = decode_wav(&wav_bytes(1, 1, 96000, 32, &d32)).unwrap();
        assert_eq!(r.data, vec![0.0, 0.5, -1.0]);

        let mut df = Vec::new();
        for v in [0.25f32, -0.75, 1.5] {
            df.extend_from_slice(&v.to_le_bytes());
        }
        let r = decode_wav(&wav_bytes(3, 1, 44100, 32, &df)).unwrap();
        assert_eq!(r.data, vec![0.25, -0.75, 1.5]);
    }

    #[test]
    fn rejects_bad_headers_without_panicking() {
        let good = wav_bytes(1, 1, 44100, 16, &[0, 0, 1, 0]);
        assert_eq!(decode_wav(b""), Err(WavError::NotWav));
        assert_eq!(decode_wav(&good[..11]), Err(WavError::NotWav));
        // Truncated inside the fmt chunk.
        assert_eq!(decode_wav(&good[..30]), Err(WavError::Truncated));
        // No data chunk.
        assert_eq!(decode_wav(&good[..36]), Err(WavError::Missing("data")));
        // Zero frames.
        assert!(decode_wav(&wav_bytes(1, 1, 44100, 16, &[])).is_err());
        // Bad fields.
        assert!(decode_wav(&wav_bytes(1, 0, 44100, 16, &[0, 0])).is_err());
        assert!(decode_wav(&wav_bytes(1, 6, 44100, 16, &[0; 12])).is_err());
        assert!(decode_wav(&wav_bytes(1, 1, 0, 16, &[0, 0])).is_err());
        assert!(decode_wav(&wav_bytes(1, 1, 44100, 12, &[0, 0])).is_err());
        assert!(decode_wav(&wav_bytes(2, 1, 44100, 16, &[0, 0])).is_err());
        assert!(decode_wav(&wav_bytes(3, 1, 44100, 16, &[0, 0])).is_err());
        // Wrong block alignment.
        let mut bad = good.clone();
        bad[32] = 7;
        assert_eq!(decode_wav(&bad), Err(WavError::Invalid("block alignment")));
    }

    #[test]
    fn size_fields_are_checked_against_the_file() {
        let good = wav_bytes(1, 1, 44100, 16, &[1, 0, 2, 0, 3, 0]);
        // A data size far past the end of the file plays what is there.
        let mut big = good.clone();
        big[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        let r = decode_wav(&big).unwrap();
        assert_eq!(r.frames(), 3);
        // A trailing odd byte is not a frame.
        let mut odd = good.clone();
        odd.push(9);
        odd[40..44].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(decode_wav(&odd).unwrap().frames(), 3);
        // An oversize chunk before the data is a truncation.
        let mut junk = good[..12].to_vec();
        junk.extend_from_slice(b"LIST");
        junk.extend_from_slice(&u32::MAX.to_le_bytes());
        junk.extend_from_slice(&good[12..]);
        assert_eq!(decode_wav(&junk), Err(WavError::Truncated));
        // An odd-sized chunk is skipped with its pad byte.
        let mut pad = good[..12].to_vec();
        pad.extend_from_slice(b"junk");
        pad.extend_from_slice(&3u32.to_le_bytes());
        pad.extend_from_slice(&[0, 0, 0, 0]);
        pad.extend_from_slice(&good[12..]);
        assert_eq!(decode_wav(&pad).unwrap().frames(), 3);
    }

    #[test]
    fn extensible_float_is_read() {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&44100u32.to_le_bytes());
        fmt.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        fmt.extend_from_slice(&4u16.to_le_bytes());
        fmt.extend_from_slice(&32u16.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&32u16.to_le_bytes());
        fmt.extend_from_slice(&4u32.to_le_bytes());
        fmt.extend_from_slice(&3u16.to_le_bytes());
        fmt.extend_from_slice(&[0u8; 14]);
        assert_eq!(fmt.len(), 40);
        let mut v = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&fmt);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&4u32.to_le_bytes());
        v.extend_from_slice(&1.0f32.to_le_bytes());
        let r = decode_wav(&v).unwrap();
        assert_eq!(r.data, vec![1.0]);
    }

    /// Deterministic fuzzing: random byte flips, truncations and size
    /// rewrites of valid files never panic, and a successful decode never
    /// holds more samples than the file has bytes.
    #[test]
    fn fuzzed_files_never_panic() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut data = Vec::new();
        for i in 0..64i16 {
            data.extend_from_slice(&(i * 300).to_le_bytes());
        }
        let bases = [
            wav_bytes(1, 1, 44100, 16, &data),
            wav_bytes(1, 2, 48000, 16, &data),
            wav_bytes(3, 1, 44100, 32, &[0u8; 64]),
            wav_bytes(1, 1, 44100, 24, &data[..60]),
        ];
        for _ in 0..20000 {
            let mut v = bases[(next() % 4) as usize].clone();
            match next() % 4 {
                0 => {
                    for _ in 0..(next() % 6) {
                        let i = (next() as usize) % v.len();
                        v[i] = next() as u8;
                    }
                }
                1 => v.truncate((next() as usize) % (v.len() + 1)),
                2 => {
                    let i = (next() as usize) % (v.len().saturating_sub(4).max(1));
                    let w = (next() as u32).to_le_bytes();
                    for (k, b) in w.iter().enumerate() {
                        if i + k < v.len() {
                            v[i + k] = *b;
                        }
                    }
                }
                _ => {
                    let extra = (next() % 40) as usize;
                    v.extend((0..extra).map(|_| next() as u8));
                }
            }
            if let Ok(d) = decode_wav(&v) {
                assert!(d.data.len() <= v.len());
                assert!(d.data.iter().all(|x| x.is_finite()));
            }
        }
    }
}
