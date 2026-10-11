use audiostrike::{Project, WavData, read_wav, write_wav_float};
use audiostrike::stream::RecordingWriter;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    scratch: PathBuf,
    root: PathBuf,
    media: PathBuf,
    project: Project,
}
impl Fixture {
    fn new(samples: Vec<f32>) -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "audiostrike-export-safety-{}-{nonce}-{}", std::process::id(),
            NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&scratch).unwrap();
        let input = scratch.join("input.wav");
        write_wav_float(&input, &WavData { sample_rate: 48_000, channels: 1, samples }).unwrap();
        let root = scratch.join("session.audiostrike");
        let mut project = Project::create(&root, 120).unwrap();
        let id = project.import_wav(&input).unwrap();
        let track = project.add_track("Test").unwrap();
        project.place_clip(track, id, 0).unwrap();
        project.save().unwrap();
        Self { media: root.join("media").join(format!("{id}.wav")), scratch, root, project }
    }
    fn assert_no_export_staging(&self) {
        assert!(!fs::read_dir(&self.scratch).unwrap().any(|entry| {
            entry.unwrap().file_name().to_string_lossy().starts_with(".audiostrike-export-")
        }));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.scratch); }
}

#[test]
fn bounce_publishes_new_wav_and_refuses_an_occupied_third_party_file() {
    let fixture = Fixture::new(vec![0.25; 80]);
    let output = fixture.scratch.join("mix.wav");
    fixture.project.render_wav(&output).unwrap();
    assert_eq!(read_wav(&output).unwrap().frames(), 80);
    let first = fs::read(&output).unwrap();
    assert!(fixture.project.render_wav(&output).is_err());
    assert_eq!(fs::read(&output).unwrap(), first);

    let sentinel = fixture.scratch.join("sentinel.wav");
    fs::write(&sentinel, b"KEEP THIS THIRD-PARTY FILE").unwrap();
    assert!(fixture.project.render_wav(&sentinel).is_err());
    assert_eq!(fs::read(&sentinel).unwrap().as_slice(), b"KEEP THIS THIRD-PARTY FILE");
    assert!(fixture.project.render_wav(&fixture.scratch.join("wrong.json")).is_err());
    fixture.assert_no_export_staging();
}

#[test]
fn project_manifest_media_recovery_and_parent_aliases_cannot_be_bounced_over() {
    let fixture = Fixture::new(vec![0.25; 80]);
    let manifest = fixture.root.join("project.json");
    let manifest_before = fs::read(&manifest).unwrap();
    let media_before = fs::read(&fixture.media).unwrap();
    let recovery = fixture.root.join("project.json.bak");
    fs::write(&recovery, b"RECOVERY SENTINEL").unwrap();

    let protected = [
        manifest.clone(), fixture.media.clone(), recovery.clone(),
        fixture.root.join("media").join("new.wav"),
        fixture.root.join("media").join("..").join("alias.wav"),
    ];
    for path in &protected {
        assert!(fixture.project.render_wav(path).is_err(), "{}", path.display());
    }
    assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
    assert_eq!(fs::read(&fixture.media).unwrap(), media_before);
    assert_eq!(fs::read(&recovery).unwrap().as_slice(), b"RECOVERY SENTINEL");
    assert!(!fixture.root.join("media").join("new.wav").exists());
    assert!(Project::open(&fixture.root).is_ok());
    fixture.assert_no_export_staging();
}

#[test]
fn hard_link_aliases_of_manifest_and_media_are_not_replaced() {
    let fixture = Fixture::new(vec![0.25; 80]);
    for (source, alias) in [
        (fixture.root.join("project.json"), fixture.scratch.join("manifest-alias.wav")),
        (fixture.media.clone(), fixture.scratch.join("media-alias.wav")),
    ] {
        fs::hard_link(&source, &alias).unwrap();
        let before = fs::read(&source).unwrap();
        assert!(fixture.project.render_wav(&alias).is_err());
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(fs::read(&alias).unwrap(), before);
    }
    fixture.assert_no_export_staging();
}

#[test]
fn recording_replacement_cannot_target_project_data_either() {
    let fixture = Fixture::new(vec![0.25; 80]);
    let manifest = fixture.root.join("project.json");
    let manifest_before = fs::read(&manifest).unwrap();
    let media_before = fs::read(&fixture.media).unwrap();
    for path in [
        manifest, fixture.media.clone(), fixture.root.join("project.json.bak"),
        fixture.root.join("other.wav"),
    ] {
        assert!(RecordingWriter::new(&path, 1).is_err(), "{}", path.display());
    }
    assert_eq!(fs::read(fixture.root.join("project.json")).unwrap(), manifest_before);
    assert_eq!(fs::read(&fixture.media).unwrap(), media_before);
}

#[test]
fn a_late_mix_failure_leaves_no_destination_or_staging_file() {
    let mut samples = vec![0.1; 5_000];
    samples[4_096..].fill(0.9);
    let mut fixture = Fixture::new(samples);
    let track = fixture.project.session().tracks[0].id;
    fixture.project.set_track_gain_db(track, 6.0).unwrap();
    fixture.project.save().unwrap();
    let output = fixture.scratch.join("failed.wav");
    assert!(fixture.project.render_wav(&output).is_err());
    assert!(!output.exists());
    fixture.assert_no_export_staging();
}

#[cfg(windows)]
fn directory_alias(source: &Path, alias: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(source, alias)
}
#[cfg(unix)]
fn directory_alias(source: &Path, alias: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, alias)
}

#[test]
fn a_directory_link_into_the_project_is_rejected_when_links_are_available() {
    let fixture = Fixture::new(vec![0.25; 80]);
    let alias = fixture.scratch.join("linked-project");
    if directory_alias(&fixture.root, &alias).is_err() {
        // Windows accounts without symlink permission still exercise parent aliases via `..` above.
        return;
    }
    assert!(fixture.project.render_wav(&alias.join("fresh.wav")).is_err());
    assert!(!fixture.root.join("fresh.wav").exists());
    fixture.assert_no_export_staging();
}
