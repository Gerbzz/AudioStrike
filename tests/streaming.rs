use audiostrike::stream::{RecordingWriter, StreamingMixer};
use audiostrike::{Project, WavData, read_wav, write_wav_float};

#[test]
fn disk_backed_mix_matches_existing_mix_across_small_chunks() {
    let root = std::env::temp_dir().join(format!("audiostrike-stream-{}.audiostrike", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let source = root.with_extension("source.wav");
    let samples = (0..12_345).map(|i| (i as f32 * 0.013).sin() * 0.18).collect();
    write_wav_float(&source, &WavData { sample_rate: 48_000, channels: 1, samples }).unwrap();
    let mut project = Project::create(&root, 120).unwrap();
    let media = project.import_wav(&source).unwrap();
    let first = project.add_track("Lead").unwrap();
    let clip = project.place_clip(first, media, 250).unwrap();
    project.set_clip_fades(clip, 100, 80).unwrap();
    project.set_track_pan(first, -0.4).unwrap();
    let second = project.add_track("Double").unwrap();
    project.place_clip(second, media, 900).unwrap();
    project.set_track_gain_db(second, -8.0).unwrap();
    project.save().unwrap();
    let reference = project.render_samples().unwrap();
    let mut stream = StreamingMixer::open(&project).unwrap();
    let mut actual = Vec::new();
    let mut at = 0;
    let mut block = vec![0.0; 127 * 2];
    loop {
        let frames = stream.read_frames(at, &mut block).unwrap();
        if frames == 0 { break; }
        actual.extend_from_slice(&block[..frames * 2]);
        at += frames as u64;
    }
    assert_eq!(reference.len(), actual.len());
    for (expected, got) in reference.iter().zip(&actual) {
        assert!((expected - got).abs() < 1e-6, "{expected} != {got}");
    }
    let destination = root.with_extension("streamed.wav");
    project.render_wav_streaming(&destination).unwrap();
    let exported = read_wav(&destination).unwrap();
    assert_eq!(exported.frames(), reference.len() / 2);
    for (expected, got) in reference.iter().zip(&exported.samples) {
        assert!((expected - got).abs() <= 2.0 / 8_388_608.0);
    }
}

#[test]
fn synthetic_recording_writes_valid_wav_and_rejects_bad_samples() {
    let destination = std::env::temp_dir().join(format!("audiostrike-record-{}.wav", std::process::id()));
    let _ = std::fs::remove_file(&destination);
    let mut writer = RecordingWriter::new(&destination, 2).unwrap();
    writer.push(&[0.25, -0.25, 0.5, -0.5]).unwrap();
    assert!(writer.push(&[f32::NAN, 0.0]).is_err());
    writer.push(&[0.75, -0.75]).unwrap();
    assert_eq!(writer.finish().unwrap(), 3);
    let wav = read_wav(&destination).unwrap();
    assert_eq!(wav.sample_rate, 48_000);
    assert_eq!(wav.channels, 2);
    assert_eq!(wav.samples, vec![0.25, -0.25, 0.5, -0.5, 0.75, -0.75]);
    let root = destination.with_extension("audiostrike");
    let _ = std::fs::remove_dir_all(&root);
    let mut project = Project::create(&root, 120).unwrap();
    let asset = project.import_wav(&destination).unwrap();
    let track = project.add_track("Recorded Test Input").unwrap();
    project.place_clip(track, asset, 0).unwrap();
    project.save().unwrap();
    let reopened = Project::open(&root).unwrap();
    for (rendered, recorded) in reopened.render_samples().unwrap().iter().zip(&wav.samples) {
        assert!((rendered - recorded).abs() < 1e-6);
    }
}
