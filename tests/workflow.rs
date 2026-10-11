use audiostrike::{Project, WavData, read_wav};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

fn temp_project(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("audiostrike-{label}-{}-{nonce}-{}.audiostrike",
        std::process::id(), NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)))
}

fn write_mono(path: &Path, samples: &[f32]) {
    audiostrike::write_wav_float(
        path,
        &WavData {
            sample_rate: 48_000,
            channels: 1,
            samples: samples.to_vec(),
        },
    )
    .unwrap();
}

#[test]
fn two_tracks_edit_save_reopen_and_render() {
    let root = temp_project("mix");
    let input_a = root.with_extension("a.wav");
    let input_b = root.with_extension("b.wav");
    write_mono(&input_a, &[0.25, 0.5, -0.5, 0.25]);
    write_mono(&input_b, &[0.125, 0.25, 0.125, 0.0]);

    let mut project = Project::create(&root, 120).unwrap();
    let a = project.import_wav(&input_a).unwrap();
    let b = project.import_wav(&input_b).unwrap();
    let t1 = project.add_track("Voice").unwrap();
    let t2 = project.add_track("Music").unwrap();
    let clip_a = project.place_clip(t1, a, 0).unwrap();
    project.place_clip(t2, b, 1).unwrap();
    project.split_clip(clip_a, 2).unwrap();
    project.save().unwrap();

    let reopened = Project::open(&root).unwrap();
    assert_eq!(reopened.session().tracks.len(), 2);
    assert_eq!(reopened.session().tracks[0].clips.len(), 2);
    let out = root.with_extension("mix.wav");
    reopened.render_wav(&out).unwrap();
    let wav = read_wav(&out).unwrap();
    assert_eq!(wav.channels, 2);
    assert_eq!(wav.sample_rate, 48_000);
    assert_eq!(wav.frames(), 5);
    // Mono clips land at equal power in stereo. Both clips overlap at frame 1.
    assert!((wav.samples[2] - (0.5 + 0.125) * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.000_01);
}

#[test]
fn invalid_import_is_transactional_and_missing_media_is_reported() {
    let root = temp_project("invalid");
    let bad = root.with_extension("bad.wav");
    std::fs::write(&bad, b"RIFFtoo short").unwrap();
    let mut project = Project::create(&root, 120).unwrap();
    assert!(project.import_wav(&bad).is_err());
    assert!(project.session().media.is_empty());
    let valid = root.with_extension("good.wav");
    write_mono(&valid, &[0.1, 0.2]);
    let asset = project.import_wav(&valid).unwrap();
    project.save().unwrap();
    let media_path = root.join("media").join(format!("{asset}.wav"));
    std::fs::remove_file(media_path).unwrap();
    let reopened = Project::open(&root).unwrap();
    assert_eq!(reopened.offline_media(), vec![asset]);
    assert!(
        reopened
            .render_wav(&root.with_extension("offline.wav"))
            .is_err()
    );
}

#[test]
fn move_undo_redo_survives_reopen() {
    let root = temp_project("undo");
    let input = root.with_extension("source.wav");
    write_mono(&input, &[0.1, 0.2]);
    let mut project = Project::create(&root, 100).unwrap();
    let asset = project.import_wav(&input).unwrap();
    let track = project.add_track("A").unwrap();
    let clip = project.place_clip(track, asset, 0).unwrap();
    project.move_clip(clip, 48).unwrap();
    project.undo().unwrap();
    assert_eq!(project.session().tracks[0].clips[0].start, 0);
    project.save().unwrap();
    let mut reopened = Project::open(&root).unwrap();
    reopened.redo().unwrap();
    assert_eq!(reopened.session().tracks[0].clips[0].start, 48);
}

#[test]
fn resamples_44100_and_rejects_nonfinite_samples() {
    let root = temp_project("resample");
    let wav = root.with_extension("441.wav");
    audiostrike::write_wav_float(
        &wav,
        &WavData {
            sample_rate: 44_100,
            channels: 1,
            samples: (0..4_410).map(|i| (i as f32 / 10.0).sin() * 0.5).collect(),
        },
    )
    .unwrap();
    let mut project = Project::create(&root, 120).unwrap();
    let asset = project.import_wav(&wav).unwrap();
    assert_eq!(project.session().media[0].frames, 4_800);
    let canonical = read_wav(&root.join("media").join(format!("{asset}.wav"))).unwrap();
    assert_eq!(canonical.frames(), 4_800);
    let mut bytes = std::fs::read(&wav).unwrap();
    bytes[44..48].copy_from_slice(&f32::NAN.to_le_bytes());
    std::fs::write(&wav, bytes).unwrap();
    assert!(project.import_wav(&wav).is_err());
    assert_eq!(project.session().media.len(), 1);
}

#[test]
fn over_peak_export_preserves_old_destination() {
    let root = temp_project("peak");
    let input = root.with_extension("hot.wav");
    write_mono(&input, &[1.0, 1.0]);
    let mut project = Project::create(&root, 120).unwrap();
    let asset = project.import_wav(&input).unwrap();
    let a = project.add_track("Hot A").unwrap();
    let b = project.add_track("Hot B").unwrap();
    project.place_clip(a, asset, 0).unwrap();
    project.place_clip(b, asset, 0).unwrap();
    let out = root.with_extension("old.wav");
    std::fs::write(&out, b"keep the old file").unwrap();
    assert!(project.render_wav(&out).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), b"keep the old file");
}

#[test]
fn save_refuses_existing_recovery_sidecars_without_changing_any_file() {
    let root = temp_project("save-sidecars");
    let mut project = Project::create(&root, 120).unwrap();
    let original = std::fs::read(root.join("project.json")).unwrap();
    std::fs::write(root.join("project.json.tmp"), b"unfinished edit sentinel").unwrap();
    std::fs::write(root.join("project.json.bak"), b"older recovery sentinel").unwrap();
    project.add_track("Uncommitted").unwrap();
    assert!(project.save().unwrap_err().to_string().contains("recover"));
    assert_eq!(std::fs::read(root.join("project.json")).unwrap(), original);
    assert_eq!(std::fs::read(root.join("project.json.tmp")).unwrap(), b"unfinished edit sentinel");
    assert_eq!(std::fs::read(root.join("project.json.bak")).unwrap(), b"older recovery sentinel");
}

#[test]
fn oversized_media_metadata_is_rejected_without_panicking_or_touching_source() {
    let root = temp_project("media-overflow");
    let input = root.with_extension("source.wav");
    write_mono(&input, &[0.25]);
    let mut project = Project::create(&root, 120).unwrap();
    project.import_wav(&input).unwrap();
    project.save().unwrap();
    let manifest = root.join("project.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    value["session"]["media"][0]["frames"] = serde_json::json!(u64::MAX);
    let malformed = serde_json::to_vec(&value).unwrap();
    std::fs::write(&manifest, &malformed).unwrap();
    assert!(Project::open(&root).is_err());
    assert_eq!(std::fs::read(&manifest).unwrap(), malformed);
}

#[test]
fn external_manifest_edit_blocks_save_and_keeps_local_edits() {
    let root = temp_project("external-save");
    let mut project = Project::create(&root, 120).unwrap();
    let manifest = root.join("project.json");
    let mut external: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    external["session"]["tempo_bpm"] = serde_json::json!(121);
    let external_bytes = serde_json::to_vec_pretty(&external).unwrap();
    std::fs::write(&manifest, &external_bytes).unwrap();
    project.add_track("Local unsaved track").unwrap();
    assert!(project.save().unwrap_err().to_string().contains("changed on disk"));
    assert_eq!(std::fs::read(&manifest).unwrap(), external_bytes);
    assert_eq!(project.session().tracks[0].name, "Local unsaved track");
    assert!(!root.join("project.json.tmp").exists());
    assert!(!root.join("project.json.bak").exists());
}

#[test]
fn import_preserves_old_temp_and_unreferenced_final_sentinels() {
    let root = temp_project("media-sentinels");
    let source = root.with_extension("source.wav");
    write_mono(&source, &[0.25, -0.25]);
    let mut project = Project::create(&root, 120).unwrap();
    let media_dir = root.join("media");
    let old_temp = media_dir.join("1.tmp");
    std::fs::write(&old_temp, b"unfinished external import").unwrap();
    assert_eq!(project.import_wav(&source).unwrap(), 1);
    assert_eq!(std::fs::read(&old_temp).unwrap(), b"unfinished external import");
    let first_media = std::fs::read(media_dir.join("1.wav")).unwrap();
    let old_final = media_dir.join("2.wav");
    std::fs::write(&old_final, b"unreferenced existing media").unwrap();
    assert!(project.import_wav(&source).is_err());
    assert_eq!(std::fs::read(&old_final).unwrap(), b"unreferenced existing media");
    assert_eq!(std::fs::read(media_dir.join("1.wav")).unwrap(), first_media);
    assert_eq!(project.session().media.len(), 1);
    assert_eq!(project.session().next_id, 2);
    assert!(std::fs::read_dir(media_dir).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().contains(".audiostrike-import-")));
}

#[test]
fn malformed_and_colliding_project_ids_preserve_referenced_media() {
    let root = temp_project("id-collision");
    let source = root.with_extension("source.wav");
    write_mono(&source, &[0.5]);
    let mut project = Project::create(&root, 120).unwrap();
    let media_id = project.import_wav(&source).unwrap();
    project.add_track("Track").unwrap();
    project.save().unwrap();
    let media_file = root.join("media").join(format!("{media_id}.wav"));
    let media_bytes = std::fs::read(&media_file).unwrap();
    let manifest = root.join("project.json");
    let clean: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    let mut colliding_next = clean.clone();
    colliding_next["session"]["next_id"] = serde_json::json!(media_id);
    std::fs::write(&manifest, serde_json::to_vec(&colliding_next).unwrap()).unwrap();
    assert!(Project::open(&root).is_err());
    assert_eq!(std::fs::read(&media_file).unwrap(), media_bytes);

    let mut duplicate = clean.clone();
    duplicate["session"]["tracks"][0]["id"] = serde_json::json!(media_id);
    std::fs::write(&manifest, serde_json::to_vec(&duplicate).unwrap()).unwrap();
    assert!(Project::open(&root).is_err());
    assert_eq!(std::fs::read(&media_file).unwrap(), media_bytes);

    let mut zero = clean.clone();
    zero["session"]["next_id"] = serde_json::json!(0);
    std::fs::write(&manifest, serde_json::to_vec(&zero).unwrap()).unwrap();
    assert!(Project::open(&root).is_err());
    assert_eq!(std::fs::read(&media_file).unwrap(), media_bytes);

    let mut exhausted = clean;
    exhausted["session"]["next_id"] = serde_json::json!(u64::MAX);
    std::fs::write(&manifest, serde_json::to_vec(&exhausted).unwrap()).unwrap();
    let mut exhausted_project = Project::open(&root).unwrap();
    assert!(exhausted_project.import_wav(&source).is_err());
    assert!(exhausted_project.add_track("Cannot allocate").is_err());
    assert_eq!(std::fs::read(&media_file).unwrap(), media_bytes);
}
