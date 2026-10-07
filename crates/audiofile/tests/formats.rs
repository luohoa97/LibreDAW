// SPDX-License-Identifier: GPL-3.0-or-later
//! Every supported format decodes to the same 440 Hz sine. The FLAC, Ogg
//! Vorbis, WavPack and MP3 fixtures are 0.2 s sines made for this test
//! (ASSETS.md); the WAV variants are built here.

use audiofile::{Error, Limits, decode_bytes, decode_bytes_with};

const FLAC: &[u8] = include_bytes!("fixtures/sine440.flac");
const OGG: &[u8] = include_bytes!("fixtures/sine440.ogg");
const WV: &[u8] = include_bytes!("fixtures/sine440.wv");
const MP3: &[u8] = include_bytes!("fixtures/sine440.mp3");

fn hz(data: &[f32], channels: u16, rate: u32) -> f64 {
    let ch = usize::from(channels);
    let mono: Vec<f32> = data.chunks(ch).map(|f| f[0]).collect();
    let crossings = mono
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    crossings as f64 * f64::from(rate) / mono.len() as f64
}

fn check_sine(name: &str, bytes: &[u8], ext: Option<&str>, channels: u16) {
    let a = decode_bytes(bytes, ext).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!((a.rate, a.channels), (22050, channels), "{name}");
    let frames = a.frames();
    assert!((4300..=5200).contains(&frames), "{name}: {frames} frames");
    let f = hz(&a.data, a.channels, a.rate);
    assert!((f - 440.0).abs() < 25.0, "{name}: {f} Hz");
    let peak = a.data.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.05 && peak <= 1.01, "{name}: peak {peak}");
}

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn riff(chunks: Vec<Vec<u8>>) -> Vec<u8> {
    let mut body = b"WAVE".to_vec();
    chunks.into_iter().for_each(|c| body.extend(c));
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(body.len() as u32).to_le_bytes());
    w.extend(body);
    w
}

fn pcm_wav(bits: u16, float: bool) -> Vec<u8> {
    let rate = 22050u32;
    let mut d = Vec::new();
    for i in 0..4410 {
        let s = (2.0 * std::f64::consts::PI * 440.0 * f64::from(i) / f64::from(rate)).sin() * 0.5;
        match (bits, float) {
            (8, _) => d.push((s * 127.0 + 128.0) as u8),
            (16, _) => d.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes()),
            (24, _) => d.extend_from_slice(&((s * 8_388_607.0) as i32).to_le_bytes()[..3]),
            (32, false) => d.extend_from_slice(&((s * 2_147_483_647.0) as i32).to_le_bytes()),
            _ => d.extend_from_slice(&(s as f32).to_le_bytes()),
        }
    }
    let mut fmt = Vec::new();
    let block = bits / 8;
    fmt.extend_from_slice(&(if float { 3u16 } else { 1 }).to_le_bytes());
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    fmt.extend_from_slice(&block.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    riff(vec![chunk(b"fmt ", &fmt), chunk(b"data", &d)])
}

/// Ogg Vorbis inside WAV, as FL Studio writes it. Tag 0x674F: Ogg pages in
/// the data chunk. Tag 0x6750: the same, with a long `fmt ` extension and a
/// `fact` chunk.
fn vorbis_wav(tag: u16) -> Vec<u8> {
    let mut fmt = Vec::new();
    for v in [tag, 1u16] {
        fmt.extend_from_slice(&v.to_le_bytes());
    }
    fmt.extend_from_slice(&22050u32.to_le_bytes());
    fmt.extend_from_slice(&8000u32.to_le_bytes());
    fmt.extend_from_slice(&[1, 0, 16, 0]);
    let extra: Vec<u8> = if tag == 0x6750 {
        OGG[..300].to_vec()
    } else {
        vec![8; 8]
    };
    fmt.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    fmt.extend(extra);
    riff(vec![
        chunk(b"fmt ", &fmt),
        chunk(b"fact", &4410u32.to_le_bytes()),
        chunk(b"data", OGG),
        chunk(b"LIST", b"adtl"),
    ])
}

#[test]
fn every_format_decodes() {
    check_sine("flac", FLAC, Some("flac"), 1);
    check_sine("ogg", OGG, Some("ogg"), 2);
    check_sine("wavpack", WV, Some("wv"), 1);
    check_sine("mp3", MP3, Some("mp3"), 1);
    for (bits, float) in [
        (8, false),
        (16, false),
        (24, false),
        (32, false),
        (32, true),
    ] {
        check_sine(
            &format!("wav{bits}{float}"),
            &pcm_wav(bits, float),
            Some("wav"),
            1,
        );
    }
    check_sine("vorbis-in-wav 674F", &vorbis_wav(0x674F), Some("wav"), 2);
    check_sine("vorbis-in-wav 6750", &vorbis_wav(0x6750), Some("wav"), 2);
}

#[test]
fn format_is_found_by_content_not_extension() {
    check_sine("flac as .bin", FLAC, Some("bin"), 1);
    check_sine("ogg no hint", OGG, None, 2);
    check_sine("wav as .ogg", &pcm_wav(16, false), Some("ogg"), 1);
}

#[test]
fn wavpack_with_a_trailing_tag() {
    let mut b = WV.to_vec();
    b.extend_from_slice(b"APETAGEX\xd0\x07\0\0 \0\0\0\0\0\0\0\0\0\0\xa0\0\0\0\0\0\0\0\0\0\0\0");
    check_sine("wv+ape", &b, Some("wv"), 1);
}

#[test]
fn vorbis_in_wav_mode_three_is_refused_clearly() {
    let mut w = vorbis_wav(0x6751);
    let i = w.windows(4).position(|x| x == b"OggS").unwrap();
    w[i] = b'X';
    // Both Ogg copies lose their magic: nothing to unwrap.
    for j in 0..w.len().saturating_sub(4) {
        if &w[j..j + 4] == b"OggS" {
            w[j] = b'X';
        }
    }
    assert!(matches!(
        decode_bytes(&w, Some("wav")),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn damaged_files_do_not_panic() {
    for (name, bytes) in [("flac", FLAC), ("ogg", OGG), ("wv", WV), ("mp3", MP3)] {
        // Truncated at many points, and with flipped bytes.
        for cut in (0..bytes.len()).step_by(97) {
            let _ = decode_bytes(&bytes[..cut], None);
        }
        let mut b = bytes.to_vec();
        for i in (0..b.len()).step_by(53) {
            b[i] ^= 0xA5;
        }
        let _ = decode_bytes(&b, None);
        assert!(
            decode_bytes(&bytes[..4.min(bytes.len())], None).is_err(),
            "{name}"
        );
    }
    let v = vorbis_wav(0x674F);
    for cut in (0..v.len()).step_by(61) {
        let _ = decode_bytes(&v[..cut], Some("wav"));
    }
    assert!(matches!(
        decode_bytes(b"not audio at all, just text", None),
        Err(Error::Unsupported(_))
    ));
    assert!(decode_bytes(&[], None).is_err());
}

#[test]
fn limits_are_enforced() {
    let small = Limits {
        max_input_bytes: 100,
        max_samples: 1000,
    };
    assert_eq!(decode_bytes_with(FLAC, None, small), Err(Error::TooLarge));
    let few = Limits {
        max_input_bytes: 1 << 20,
        max_samples: 1000,
    };
    assert_eq!(
        decode_bytes_with(&pcm_wav(16, false), None, few),
        Err(Error::TooLarge)
    );
    assert_eq!(decode_bytes_with(WV, None, few), Err(Error::TooLarge));
}
