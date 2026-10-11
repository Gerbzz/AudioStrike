use std::process::Command;

#[test]
fn command_line_can_create_import_arrange_and_bounce() {
    let exe = env!("CARGO_BIN_EXE_audiostrike");
    let root = std::env::temp_dir().join(format!(
        "audiostrike-cli-{}.audiostrike",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let input = root.with_extension("source.wav");
    let output = root.with_extension("output.wav");
    audiostrike::write_wav_float(
        &input,
        &audiostrike::WavData {
            sample_rate: 48_000,
            channels: 1,
            samples: vec![0.2, 0.4],
        },
    )
    .unwrap();
    for args in [
        vec!["new".to_string(), root.display().to_string(), "120".into()],
        vec![
            "import".to_string(),
            root.display().to_string(),
            input.display().to_string(),
        ],
        vec![
            "track".to_string(),
            root.display().to_string(),
            "Lead Voice".into(),
        ],
        vec![
            "clip".to_string(),
            root.display().to_string(),
            "2".into(),
            "1".into(),
            "0".into(),
        ],
        vec![
            "bounce".to_string(),
            root.display().to_string(),
            output.display().to_string(),
        ],
    ] {
        let result = Command::new(exe).args(&args).output().unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(audiostrike::read_wav(&output).unwrap().frames(), 2);
}
