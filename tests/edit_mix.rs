use audiostrike::{Project, WavData, read_wav, write_wav_float};

#[test]
fn fader_pan_fade_and_playback_mix_match_export() {
    let root = std::env::temp_dir().join(format!(
        "audiostrike-mixer-{}.audiostrike",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let wav = root.with_extension("source.wav");
    write_wav_float(
        &wav,
        &WavData {
            sample_rate: 48_000,
            channels: 1,
            samples: vec![0.4; 8],
        },
    )
    .unwrap();
    let mut project = Project::create(&root, 120).unwrap();
    let media = project.import_wav(&wav).unwrap();
    let track = project.add_track("Lead").unwrap();
    let clip = project.place_clip(track, media, 0).unwrap();
    project.set_track_gain_db(track, -6.0).unwrap();
    project.set_track_pan(track, -1.0).unwrap();
    project.set_clip_fades(clip, 2, 2).unwrap();
    let mix = project.render_samples().unwrap();
    assert_eq!(mix.len(), 16);
    assert_eq!(mix[0], 0.0);
    assert!(mix[4] > 0.19 && mix[4] < 0.21);
    assert!(mix.chunks_exact(2).all(|f| f[1].abs() < 0.000_001));
    project.save().unwrap();
    let reopened = Project::open(&root).unwrap();
    assert_eq!(mix, reopened.render_samples().unwrap());
    let out = root.with_extension("master.wav");
    reopened.render_wav(&out).unwrap();
    let exported = read_wav(&out).unwrap();
    for (sample, rendered) in exported.samples.iter().zip(&mix) {
        assert!((sample - rendered).abs() <= 1.0 / 8_388_608.0);
    }
    project.set_track_gain_db(track, 15.0).unwrap();
    project.save().unwrap();
    let peak_fader = Project::open(&root).unwrap();
    assert!(peak_fader.session().tracks[0].gain > 5.62);
}

#[test]
fn filmcraft_ui_law_keeps_zoom_anchor_and_maps_unity() {
    use audiostrike::donor::filmcraft_ui::{TimelineView, db_to_pos, pos_to_db, zoom_about};
    assert!((db_to_pos(0.0) - 0.76).abs() < 1e-6);
    assert!((pos_to_db(0.76)).abs() < 1e-6);
    assert!((pos_to_db(1.0) - 15.0).abs() < 1e-6);
    assert!((db_to_pos(15.0) - 1.0).abs() < 1e-6);
    let mut view = TimelineView {
        target_pps: 100.0,
        target_scroll: 1.0,
    };
    zoom_about(&mut view, 2.0, 3.0, 800.0);
    assert!((view.target_pps - 200.0).abs() < 1e-6);
    assert!((view.target_scroll - 2.0).abs() < 1e-6);
}
