// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio file headers: sample rate, channels and length without decoding
//! (WAV, FLAC, Ogg Vorbis, WavPack).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Wav,
    Flac,
    Ogg,
    Wavpack,
    /// Ogg Vorbis inside a WAV container (format tag 0x674F), which is how
    /// most of FL Studio's own `.wav` files are stored.
    #[serde(rename = "wav-vorbis")]
    WavVorbis,
}

impl Format {
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "wav" | "wave" => Some(Format::Wav),
            "flac" => Some(Format::Flac),
            "ogg" | "oga" => Some(Format::Ogg),
            "wv" => Some(Format::Wavpack),
            _ => None,
        }
    }

    pub fn of_path(path: &Path) -> Option<Format> {
        Format::from_ext(path.extension()?.to_str()?)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Ogg => "ogg",
            Format::Wavpack => "wavpack",
            Format::WavVorbis => "wav-vorbis",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    pub format: Format,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Reads only the header of `path`; the format comes from the extension.
pub fn read_header(path: &Path) -> io::Result<AudioHeader> {
    let format = Format::of_path(path).ok_or_else(|| bad("not an audio file"))?;
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    match format {
        Format::Wav | Format::WavVorbis => {
            let l = wav_layout(&mut f, len)?;
            if matches!(l.tag, 0x674E..=0x6751) {
                // The Ogg stream inside is the truth: the `fmt ` channel count and
                // the `fact` length are often wrong in FL's files. Its last page
                // gives the length (an upper bound: some streams end early).
                let frames = ogg_last_granule(&mut f, l.data_pos, l.data_len)?;
                let mut b = [0u8; 128];
                f.seek(SeekFrom::Start(l.data_pos))?;
                let n = f.read(&mut b)?;
                let (channels, sample_rate) =
                    vorbis_ident(&b[..n]).unwrap_or((l.channels, l.sample_rate));
                return Ok(AudioHeader {
                    format: Format::WavVorbis,
                    sample_rate,
                    channels,
                    frames,
                });
            }
            Ok(AudioHeader {
                format,
                sample_rate: l.sample_rate,
                channels: l.channels,
                frames: l.data_len / u64::from(l.block_align.max(1)),
            })
        }
        Format::Flac => flac_header(&mut f),
        Format::Ogg => ogg_header(&mut f, len),
        Format::Wavpack => wavpack_header(&mut f),
    }
}

/// Where the samples are in a WAV file.
#[derive(Clone, Copy, Debug)]
pub struct WavLayout {
    /// 1 = integer PCM, 3 = float (also resolved through WAVE_FORMAT_EXTENSIBLE).
    pub tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
    pub bits: u16,
    pub data_pos: u64,
    pub data_len: u64,
}

pub fn wav_layout<R: Read + Seek>(r: &mut R, len: u64) -> io::Result<WavLayout> {
    let mut h = [0u8; 12];
    r.read_exact(&mut h)?;
    if (&h[0..4] != b"RIFF" && &h[0..4] != b"RF64") || &h[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None;
    let mut pos = 12u64;
    for _ in 0..256 {
        let mut ch = [0u8; 8];
        if r.read_exact(&mut ch).is_err() {
            break;
        }
        let size = u64::from(le32(&ch[4..8]));
        pos += 8;
        if &ch[0..4] == b"fmt " {
            let n = size.min(40) as usize;
            if n < 16 {
                return Err(bad("short fmt chunk"));
            }
            let mut b = vec![0u8; n];
            r.read_exact(&mut b)?;
            let mut tag = le16(&b[0..2]);
            if tag == 0xFFFE && n >= 26 {
                tag = le16(&b[24..26]);
            }
            fmt = Some((
                tag,
                le16(&b[2..4]),
                le32(&b[4..8]),
                le16(&b[12..14]),
                le16(&b[14..16]),
            ));
            let adv = size - n as u64 + (size & 1);
            r.seek(SeekFrom::Current(adv as i64))?;
            pos += size + (size & 1);
        } else if &ch[0..4] == b"data" {
            let (tag, channels, sample_rate, block_align, bits) =
                fmt.ok_or_else(|| bad("data before fmt"))?;
            let remain = len.saturating_sub(pos);
            let data_len = if size == 0xFFFF_FFFF || size > remain {
                remain
            } else {
                size
            };
            return Ok(WavLayout {
                tag,
                channels,
                sample_rate,
                block_align,
                bits,
                data_pos: pos,
                data_len,
            });
        } else {
            let adv = size + (size & 1);
            r.seek(SeekFrom::Current(adv as i64))?;
            pos += adv;
            if pos >= len {
                break;
            }
        }
    }
    Err(bad("no data chunk"))
}

fn flac_header(f: &mut File) -> io::Result<AudioHeader> {
    let mut b = [0u8; 26];
    f.read_exact(&mut b)?;
    if &b[0..4] != b"fLaC" || b[4] & 0x7f != 0 {
        return Err(bad("no FLAC STREAMINFO"));
    }
    let s = &b[8..];
    let sample_rate = (u32::from(s[10]) << 12) | (u32::from(s[11]) << 4) | (u32::from(s[12]) >> 4);
    let channels = u16::from((s[12] >> 1) & 7) + 1;
    let frames = (u64::from(s[13] & 0x0f) << 32) | u64::from(be32(&s[14..18]));
    Ok(AudioHeader {
        format: Format::Flac,
        sample_rate,
        channels,
        frames,
    })
}

fn ogg_header(f: &mut File, len: u64) -> io::Result<AudioHeader> {
    let mut b = [0u8; 128];
    let n = f.read(&mut b)?;
    if n < 64 || &b[0..4] != b"OggS" {
        return Err(bad("not an Ogg file"));
    }
    let (channels, sample_rate) = vorbis_ident(&b[..n]).ok_or_else(|| bad("not Ogg Vorbis"))?;
    let frames = ogg_last_granule(f, 0, len)?;
    Ok(AudioHeader {
        format: Format::Ogg,
        sample_rate,
        channels,
        frames,
    })
}

/// Channels and sample rate from the Vorbis identification header in the
/// first Ogg page of `b`.
fn vorbis_ident(b: &[u8]) -> Option<(u16, u32)> {
    if b.len() < 28 || &b[0..4] != b"OggS" {
        return None;
    }
    let p = 27 + usize::from(b[26]);
    if p + 16 > b.len() || &b[p..p + 7] != b"\x01vorbis" {
        return None;
    }
    Some((u16::from(b[p + 11]), le32(&b[p + 12..p + 16])))
}

/// Granule position of the last Ogg page in `[start, start+len)`: the
/// length in sample frames of a Vorbis stream.
fn ogg_last_granule(f: &mut File, start: u64, len: u64) -> io::Result<u64> {
    let tail = len.min(65536);
    f.seek(SeekFrom::Start(start + len - tail))?;
    let mut t = vec![0u8; tail as usize];
    f.read_exact(&mut t)?;
    Ok(t.windows(4)
        .rposition(|w| w == b"OggS")
        .filter(|&i| i + 14 <= t.len())
        .map(|i| i64::from_le_bytes(t[i + 6..i + 14].try_into().unwrap_or([0; 8])))
        .map_or(0, |g| g.max(0) as u64))
}

const WV_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    192000,
];

fn wavpack_header(f: &mut File) -> io::Result<AudioHeader> {
    let mut b = [0u8; 288];
    let n = f.read(&mut b)?;
    if n < 32 || &b[0..4] != b"wvpk" {
        return Err(bad("not a WavPack file"));
    }
    let total = le32(&b[12..16]);
    let flags = le32(&b[24..28]);
    let frames = if total == u32::MAX {
        0
    } else {
        u64::from(total) | (u64::from(b[11]) << 32)
    };
    let channels = if flags & 4 != 0 { 1 } else { 2 };
    let idx = ((flags >> 23) & 15) as usize;
    let sample_rate = if idx < WV_RATES.len() {
        WV_RATES[idx]
    } else {
        wv_custom_rate(&b[32..n]).ok_or_else(|| bad("WavPack: no sample rate"))?
    };
    Ok(AudioHeader {
        format: Format::Wavpack,
        sample_rate,
        channels,
        frames,
    })
}

/// ID_SAMPLE_RATE (0x27) metadata sub-block of a WavPack block.
fn wv_custom_rate(mut m: &[u8]) -> Option<u32> {
    while m.len() >= 2 {
        let id = m[0];
        let (words, head) = if id & 0x80 != 0 {
            if m.len() < 4 {
                return None;
            }
            (
                (usize::from(m[1])) | (usize::from(m[2]) << 8) | (usize::from(m[3]) << 16),
                4,
            )
        } else {
            (usize::from(m[1]), 2)
        };
        let size = words * 2;
        if id & 0x3f == 0x27 && m.len() >= head + 3 {
            return Some(
                u32::from(m[head]) | (u32::from(m[head + 1]) << 8) | (u32::from(m[head + 2]) << 16),
            );
        }
        if m.len() < head + size {
            return None;
        }
        m = &m[head + size..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, sine_wav, write};

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn vorbis_wav(fact: Option<u32>, granule: i64) -> Vec<u8> {
        let mut fmt = Vec::new();
        for v in [0x674Fu16, 1] {
            fmt.extend_from_slice(&v.to_le_bytes());
        }
        fmt.extend_from_slice(&44100u32.to_le_bytes());
        fmt.extend_from_slice(&12000u32.to_le_bytes());
        fmt.extend_from_slice(&[1, 0, 16, 0, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
        let mut page = b"OggS\0\x04".to_vec();
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&[0; 16]);
        let mut body = b"WAVE".to_vec();
        body.extend(chunk(b"fmt ", &fmt));
        if let Some(n) = fact {
            body.extend(chunk(b"fact", &n.to_le_bytes()));
        }
        body.extend(chunk(b"data", &page));
        let mut w = b"RIFF".to_vec();
        w.extend_from_slice(&(body.len() as u32).to_le_bytes());
        w.extend(body);
        w
    }

    #[test]
    fn wav_pcm() {
        let t = TempDir::new("hdr1");
        write(t.path(), "a.wav", &sine_wav(100.0, 0.5, 22050));
        let h = read_header(&t.path().join("a.wav")).unwrap();
        assert_eq!(
            (h.format, h.sample_rate, h.channels, h.frames),
            (Format::Wav, 22050, 1, 11025)
        );
    }

    #[test]
    fn vorbis_in_wav_length_comes_from_the_last_page_not_fact() {
        let t = TempDir::new("hdr2");
        write(t.path(), "f.wav", &vorbis_wav(Some(1234), 99));
        let f = read_header(&t.path().join("f.wav")).unwrap();
        assert_eq!(
            (f.format, f.frames, f.sample_rate),
            (Format::WavVorbis, 99, 44100)
        );
    }

    #[test]
    fn flac_streaminfo() {
        let t = TempDir::new("hdr3");
        let mut s = b"fLaC\0\0\0\x22".to_vec();
        let mut info = vec![0u8; 34];
        info[10] = 0x0B;
        info[11] = 0xB8;
        info[12] = 0x02; // low nibble of rate (0), channels-1 = 1 in bits 1..3
        info[13] = 0xF0;
        info[17] = 0x80; // total frames low bits
        info[16] = 0x01;
        s.extend(info);
        write(t.path(), "a.flac", &s);
        let h = read_header(&t.path().join("a.flac")).unwrap();
        assert_eq!((h.sample_rate, h.channels), (48000, 2));
        assert_eq!(h.frames, 0x0180);
    }

    #[test]
    fn wavpack_block_header() {
        let mut b = b"wvpk".to_vec();
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&[0x10, 0x04, 0, 0]);
        b.extend_from_slice(&48000u32.to_le_bytes()); // total samples
        b.extend_from_slice(&[0; 8]);
        let flags: u32 = 1 | (10 << 23); // 16-bit stereo, 48000 Hz
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        let t = TempDir::new("hdr4");
        write(t.path(), "a.wv", &b);
        let h = read_header(&t.path().join("a.wv")).unwrap();
        assert_eq!(
            (h.format, h.sample_rate, h.channels, h.frames),
            (Format::Wavpack, 48000, 2, 48000)
        );
    }
}
