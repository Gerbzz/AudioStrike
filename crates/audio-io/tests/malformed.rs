//! Hostile inputs must produce `Err` (or a best-effort `Ok`), never a panic.

use proptest::prelude::*;
use soundcraft_audio_io::{AudioBuffer, AudioError, BitDepth, EncodeOptions, FileFormat, SampleFormat, decode, detect_format, encode, probe};

fn small(format: FileFormat, depth: BitDepth, channels: usize) -> Vec<u8> {
    let mut b = AudioBuffer::new(44_100, channels, 300);
    for ch in &mut b.channels {
        for (i, s) in ch.iter_mut().enumerate() {
            *s = ((i as f32) * 0.1).sin() * 0.5;
        }
    }
    encode(&b, &EncodeOptions { format, bit_depth: depth, dither: false, bwf: None }).unwrap()
}

/// Little helper for building RIFF files by hand.
fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = b"WAVE".to_vec();
    for (id, data) in chunks {
        body.extend_from_slice(*id);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        if data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn fmt(tag: u16, channels: u16, sr: u32, bits: u16) -> Vec<u8> {
    let align = channels * bits.div_ceil(8);
    let mut f = Vec::new();
    f.extend_from_slice(&tag.to_le_bytes());
    f.extend_from_slice(&channels.to_le_bytes());
    f.extend_from_slice(&sr.to_le_bytes());
    f.extend_from_slice(&(sr * u32::from(align)).to_le_bytes());
    f.extend_from_slice(&align.to_le_bytes());
    f.extend_from_slice(&bits.to_le_bytes());
    f
}

#[test]
fn every_truncation_is_handled() {
    let files = [
        small(FileFormat::Wav, BitDepth::Int16, 2),
        small(FileFormat::Wav, BitDepth::Float32, 6),
        small(FileFormat::Aiff, BitDepth::Int24, 1),
        small(FileFormat::Aiff, BitDepth::Float32, 2),
        small(FileFormat::Flac, BitDepth::Int16, 2),
        small(FileFormat::Flac, BitDepth::Int24, 1),
    ];
    for file in &files {
        for n in 0..file.len() {
            let cut = &file[..n];
            let _ = probe(cut, None);
            if let Ok((info, buf)) = decode(cut, None) {
                assert_eq!(buf.num_channels(), info.channels as usize);
                assert_eq!(buf.frames() as u64, info.frames);
            }
        }
    }
}

#[test]
fn truncated_data_chunk_yields_partial_audio() {
    let file = small(FileFormat::Wav, BitDepth::Int16, 2);
    let (info, buf) = decode(&file[..file.len() - 400], None).unwrap();
    assert_eq!(info.frames, 200);
    assert_eq!(buf.frames(), 200);
}

#[test]
fn every_single_byte_corruption_is_handled() {
    for file in [small(FileFormat::Wav, BitDepth::Int24, 2), small(FileFormat::Aiff, BitDepth::Int16, 2), small(FileFormat::Flac, BitDepth::Int16, 1)]
    {
        for i in 0..file.len().min(200) {
            for v in [0x00, 0xFF, 0x7F, 0x80] {
                let mut f = file.clone();
                f[i] = v;
                let _ = probe(&f, None);
                let _ = decode(&f, None);
            }
        }
    }
}

#[test]
fn zero_channels_is_malformed() {
    let wav = riff(&[(b"fmt ", fmt(1, 0, 44_100, 16)), (b"data", vec![0; 16])]);
    assert!(matches!(decode(&wav, None), Err(AudioError::Malformed(_))));
    assert!(probe(&wav, None).is_err());
    let mut aiff = small(FileFormat::Aiff, BitDepth::Int16, 1);
    let comm = aiff.windows(4).position(|w| w == b"COMM").unwrap();
    aiff[comm + 8] = 0;
    aiff[comm + 9] = 0;
    assert!(matches!(decode(&aiff, None), Err(AudioError::Malformed(_))));
}

#[test]
fn zero_sample_rate_is_malformed() {
    let wav = riff(&[(b"fmt ", fmt(1, 2, 0, 16)), (b"data", vec![0; 16])]);
    assert!(decode(&wav, None).is_err());
    let mut aiff = small(FileFormat::Aiff, BitDepth::Int16, 1);
    let comm = aiff.windows(4).position(|w| w == b"COMM").unwrap();
    for b in &mut aiff[comm + 16..comm + 26] {
        *b = 0;
    }
    assert!(decode(&aiff, None).is_err());
    // Infinite exponent.
    aiff[comm + 16] = 0x7F;
    aiff[comm + 17] = 0xFF;
    assert!(decode(&aiff, None).is_err());
}

#[test]
fn absurd_chunk_sizes() {
    // fmt chunk claims 4 GiB.
    let mut wav = riff(&[(b"fmt ", fmt(1, 2, 44_100, 16)), (b"data", vec![0; 16])]);
    wav[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&wav, None).is_err());
    // data chunk claims 4 GiB: lenient read of what's there.
    let mut wav = riff(&[(b"fmt ", fmt(1, 2, 44_100, 16)), (b"data", vec![0; 16])]);
    let d = wav.windows(4).position(|w| w == b"data").unwrap();
    wav[d + 4..d + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let (info, buf) = decode(&wav, None).unwrap();
    assert_eq!((info.frames, buf.frames()), (4, 4));
    // An unknown chunk with an absurd size before fmt.
    let mut wav = riff(&[(b"junk", vec![0; 4]), (b"fmt ", fmt(1, 2, 44_100, 16)), (b"data", vec![0; 16])]);
    wav[16..20].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    assert!(decode(&wav, None).is_err());
    // RF64 with a ds64 claiming an exabyte of data.
    let mut ds64 = Vec::new();
    ds64.extend_from_slice(&u64::MAX.to_le_bytes());
    ds64.extend_from_slice(&u64::MAX.to_le_bytes());
    ds64.extend_from_slice(&u64::MAX.to_le_bytes());
    ds64.extend_from_slice(&0u32.to_le_bytes());
    let mut rf = riff(&[(b"ds64", ds64), (b"fmt ", fmt(1, 1, 8_000, 8)), (b"data", vec![0x80; 10])]);
    rf[..4].copy_from_slice(b"RF64");
    let d = rf.windows(4).rposition(|w| w == b"data").unwrap();
    rf[d + 4..d + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    let (info, buf) = decode(&rf, None).unwrap();
    assert_eq!((info.frames, buf.frames()), (10, 10));
    // AIFF COMM claims more frames than present; SSND offset beyond the end.
    let mut aiff = small(FileFormat::Aiff, BitDepth::Int16, 1);
    let comm = aiff.windows(4).position(|w| w == b"COMM").unwrap();
    aiff[comm + 10..comm + 14].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(decode(&aiff, None).unwrap().1.frames(), 300);
    let ssnd = aiff.windows(4).position(|w| w == b"SSND").unwrap();
    aiff[ssnd + 8..ssnd + 12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(decode(&aiff, None).unwrap().1.frames(), 0);
}

#[test]
fn many_tiny_chunks_terminate() {
    let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"fmt ", fmt(1, 1, 8_000, 16))];
    for _ in 0..200_000 {
        chunks.push((b"pad ", Vec::new()));
    }
    let wav = riff(&chunks);
    assert!(decode(&wav, None).is_err()); // no data chunk within the scan limit
}

#[test]
fn hand_built_wav_variants() {
    // 8-bit unsigned.
    let wav = riff(&[(b"fmt ", fmt(1, 1, 8_000, 8)), (b"data", vec![0x80, 0xFF, 0x00])]);
    let (info, buf) = decode(&wav, None).unwrap();
    assert_eq!(info.sample_format, SampleFormat::Int8);
    assert_eq!(buf.channels[0], vec![0.0, 127.0 / 128.0, -1.0]);
    // 64-bit float, with NaN replaced by silence.
    let mut data = Vec::new();
    for v in [0.25f64, -0.5, f64::NAN] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    let wav = riff(&[(b"fmt ", fmt(3, 1, 8_000, 64)), (b"data", data)]);
    let (info, buf) = decode(&wav, None).unwrap();
    assert_eq!(info.sample_format, SampleFormat::Float64);
    assert_eq!(buf.channels[0], vec![0.25, -0.5, 0.0]);
    // Unsupported PCM width.
    let wav = riff(&[(b"fmt ", fmt(3, 1, 8_000, 16)), (b"data", vec![0; 4])]);
    assert!(matches!(decode(&wav, None), Err(AudioError::Unsupported(_))));
    // No data chunk / no fmt chunk.
    assert!(decode(&riff(&[(b"fmt ", fmt(1, 1, 8_000, 16))]), None).is_err());
    assert!(decode(&riff(&[(b"data", vec![0; 4])]), None).is_err());
    // Short fmt chunk.
    assert!(decode(&riff(&[(b"fmt ", vec![1, 0, 1]), (b"data", vec![0; 4])]), None).is_err());
    // Unknown codec tag goes to symphonia and fails cleanly.
    let wav = riff(&[(b"fmt ", fmt(0x1234, 1, 8_000, 16)), (b"data", vec![0; 4])]);
    assert!(decode(&wav, None).is_err());
}

#[test]
fn aiff_sowt_little_endian() {
    let be = small(FileFormat::Aiff, BitDepth::Int16, 2);
    let (_, expect) = decode(&be, None).unwrap();
    // Rebuild as AIFC/sowt by hand.
    let comm = be.windows(4).position(|w| w == b"COMM").unwrap();
    let ssnd = be.windows(4).position(|w| w == b"SSND").unwrap();
    let mut comm_body = be[comm + 8..comm + 26].to_vec();
    comm_body.extend_from_slice(b"sowt");
    comm_body.extend_from_slice(&[0, 0]); // empty pstring, padded
    let mut ssnd_body = be[ssnd + 8..].to_vec();
    for pair in ssnd_body[8..].as_chunks_mut::<2>().0 {
        pair.swap(0, 1);
    }
    let mut body = b"AIFC".to_vec();
    for (id, data) in [(b"COMM", &comm_body), (b"SSND", &ssnd_body)] {
        body.extend_from_slice(id);
        body.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
    }
    let mut f = b"FORM".to_vec();
    f.extend_from_slice(&(body.len() as u32).to_be_bytes());
    f.extend_from_slice(&body);
    let (info, got) = decode(&f, None).unwrap();
    assert_eq!(info.sample_format, SampleFormat::Int16);
    assert_eq!(got, expect);
}

#[test]
fn detect_format_by_magic_and_hint() {
    assert_eq!(detect_format(&small(FileFormat::Wav, BitDepth::Int16, 1), Some("mp3")), FileFormat::Wav);
    assert_eq!(detect_format(&small(FileFormat::Aiff, BitDepth::Int16, 1), None), FileFormat::Aiff);
    assert_eq!(detect_format(&small(FileFormat::Flac, BitDepth::Int16, 1), None), FileFormat::Flac);
    assert_eq!(detect_format(b"OggS\0\x02rest", None), FileFormat::Ogg);
    assert_eq!(detect_format(b"caff\0\x01\0\0", None), FileFormat::Caf);
    assert_eq!(detect_format(b"ID3\x04\0\0\0\0\0\x00", None), FileFormat::Mp3);
    let mut id3_flac = b"ID3\x04\0\0\0\0\0\x02xx".to_vec();
    id3_flac.extend_from_slice(b"fLaC");
    assert_eq!(detect_format(&id3_flac, None), FileFormat::Flac);
    assert_eq!(detect_format(&[0xFF, 0xFB, 0x90, 0x00], None), FileFormat::Mp3);
    assert_eq!(detect_format(&[0xFF, 0xF1, 0x50, 0x80], None), FileFormat::Aac);
    let mut m4a = b"\0\0\0\x20ftypM4A ".to_vec();
    m4a.extend_from_slice(b"....stsd\0\0\0\0\0\0\0\x01\0\0\0\x24alac");
    assert_eq!(detect_format(&m4a, None), FileFormat::Alac);
    assert_eq!(detect_format(b"\0\0\0\x20ftypM4A ", None), FileFormat::Aac);
    assert_eq!(detect_format(b"", Some(".WAV")), FileFormat::Wav);
    assert_eq!(detect_format(b"zz", Some("aif")), FileFormat::Aiff);
    assert_eq!(detect_format(b"zz", Some("m4a")), FileFormat::Aac);
    assert_eq!(detect_format(b"zz", Some("flac")), FileFormat::Flac);
    assert_eq!(detect_format(b"zz", Some("xyz")), FileFormat::Other);
    assert_eq!(detect_format(b"zz", None), FileFormat::Other);
    assert_eq!(detect_format(b"", None), FileFormat::Other);
}

#[test]
fn garbage_in_compressed_containers_errors() {
    let garbage: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    for hint in [None, Some("mp3"), Some("ogg"), Some("flac"), Some("m4a"), Some("caf"), Some("aac"), Some("mkv"), Some("wav"), Some("aiff")] {
        assert!(decode(&garbage, hint).is_err(), "{hint:?}");
        assert!(probe(&garbage, hint).is_err(), "{hint:?}");
    }
    for magic in [&b"fLaC"[..], b"OggS", b"caff", b"ID3\x03\0\0\0\0\0\0", &[0xFF, 0xFB, 0x90, 0x00], b"\0\0\0\x20ftypM4A "] {
        let mut f = magic.to_vec();
        f.extend_from_slice(&garbage);
        let _ = probe(&f, None);
        assert!(decode(&f, None).map(|(_, b)| b.frames()).unwrap_or(0) < 1_000_000);
    }
    assert!(decode(&[], None).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048), hint in 0usize..6) {
        let hint = [None, Some("wav"), Some("aiff"), Some("flac"), Some("mp3"), Some("ogg")][hint];
        let _ = detect_format(&bytes, hint);
        let _ = probe(&bytes, hint);
        let _ = decode(&bytes, hint);
    }

    #[test]
    fn random_bodies_behind_valid_headers_never_panic(body in proptest::collection::vec(any::<u8>(), 0..1024), which in 0usize..4) {
        let mut f = match which {
            0 => b"RIFF\xff\xff\xff\xffWAVE".to_vec(),
            1 => b"RF64\xff\xff\xff\xffWAVE".to_vec(),
            2 => b"FORM\xff\xff\xff\xffAIFC".to_vec(),
            _ => b"FORM\xff\xff\xff\xffAIFF".to_vec(),
        };
        f.extend_from_slice(&body);
        let _ = probe(&f, None);
        let _ = decode(&f, None);
    }

    #[test]
    fn random_audio_roundtrips_through_flac(samples in proptest::collection::vec(-1.2f32..1.2, 1..3000), channels in 1usize..4) {
        let buf = AudioBuffer::from_interleaved(48_000, channels, &samples);
        prop_assume!(buf.frames() > 0);
        let opts = EncodeOptions { format: FileFormat::Flac, bit_depth: BitDepth::Int24, dither: false, bwf: None };
        let (_, flac) = decode(&encode(&buf, &opts).unwrap(), None).unwrap();
        let (_, wav) = decode(&encode(&buf, &EncodeOptions { format: FileFormat::Wav, ..opts }).unwrap(), None).unwrap();
        prop_assert_eq!(flac, wav);
    }
}
