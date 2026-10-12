//! One-way, conservative import of the earlier AudioStrike `.audiostrike` directory.
//! The original document and media stay untouched. The new `.scraft` references the
//! original media by absolute path, so moving or deleting the old directory breaks it.

use crate::{EngineError, Result, io::publish_new_export};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use soundcraft_audio_io::FileFormat;
use soundcraft_model::{ChannelFormat, Clip, ClipId, Fade, FadeShape, Session, Source, SourceId, Track, TrackId, TrackKind};
use soundcraft_time::{SampleRate, TempoMap};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_MEDIA_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDocument {
    session: LegacySession,
    undo: Vec<LegacySession>,
    redo: Vec<LegacySession>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySession {
    version: u32,
    sample_rate: u32,
    tempo_bpm: u16,
    next_id: u64,
    media: Vec<LegacyMedia>,
    tracks: Vec<LegacyTrack>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMedia {
    id: u64,
    frames: u64,
    channels: u16,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTrack {
    id: u64,
    name: String,
    gain: f32,
    pan: f32,
    mute: bool,
    solo: bool,
    clips: Vec<LegacyClip>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyClip {
    id: u64,
    media_id: u64,
    source_in: u64,
    len: u64,
    start: u64,
    gain: f32,
    fade_in: u64,
    fade_out: u64,
}

/// History is omitted only with the caller's explicit `current_state_only` choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionReport {
    pub sources: usize,
    pub tracks: usize,
    pub clips: usize,
    pub omitted_undo: usize,
    pub omitted_redo: usize,
    pub source_root: PathBuf,
}

fn invalid(reason: impl Into<String>) -> EngineError {
    EngineError::Unsupported(format!("legacy AudioStrike conversion: {}", reason.into()))
}

fn io(path: &Path, err: impl std::fmt::Display) -> EngineError {
    EngineError::Io(format!("{}: {err}", path.display()))
}

fn checked_sample(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid(format!("{label} exceeds the new sample position range")))
}

fn gain_db(value: f32, label: &str) -> Result<f32> {
    if !value.is_finite() || value <= 0.0 {
        return Err(invalid(format!("{label} must be finite and above zero")));
    }
    let db = 20.0 * value.log10();
    if db <= -144.0 || db > 12.0 {
        return Err(invalid(format!("{label} is outside the new -144..12 dB range")));
    }
    Ok(db)
}

fn read_document(root: &Path) -> Result<(LegacyDocument, Vec<u8>)> {
    let path = root.join("project.json");
    let metadata = fs::symlink_metadata(&path).map_err(|e| io(&path, e))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_DOCUMENT_BYTES {
        return Err(invalid("project.json must be a regular file of at most 16 MiB"));
    }
    // A completed legacy save may leave a .bak; it remains untouched. A .tmp
    // indicates uncertain in-flight recovery state and needs human resolution.
    for sidecar in ["project.json.tmp"] {
        let path = root.join(sidecar);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(invalid(format!("{sidecar} exists; resolve legacy save/recovery state first"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&path, e)),
        }
    }
    let bytes: Vec<u8> = File::open(&path)
        .map_err(|e| io(&path, e))?
        .take(MAX_DOCUMENT_BYTES + 1)
        .bytes()
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| io(&path, e))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid("project.json changed or exceeds 16 MiB"));
    }
    let document = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("project.json schema or JSON: {e}")))?;
    Ok((document, bytes))
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}
fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Check the exact canonical 48 kHz float WAV layout that the legacy mixer reads.
/// Hash and inspect samples in bounded blocks; do not trust manifest dimensions.
fn verify_media(path: &Path, media: &LegacyMedia) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|e| io(path, e))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_TOTAL_MEDIA_BYTES {
        return Err(invalid(format!("{} is not a regular canonical media file within the 256 MiB import limit", path.display())));
    }
    let mut file = File::open(path).map_err(|e| io(path, e))?;
    let mut header = [0u8; 44];
    file.read_exact(&mut header).map_err(|e| io(path, e))?;
    let data_len = media.frames.checked_mul(u64::from(media.channels)).and_then(|v| v.checked_mul(4))
        .ok_or_else(|| invalid(format!("media {} byte count overflows", media.id)))?;
    if data_len > u64::from(u32::MAX) || data_len + 44 != metadata.len()
        || &header[0..4] != b"RIFF" || &header[8..16] != b"WAVEfmt "
        || &header[36..40] != b"data" || u64::from(le32(&header, 4)) + 8 != metadata.len()
        || le32(&header, 16) != 16 || le16(&header, 20) != 3
        || le16(&header, 22) != media.channels || le32(&header, 24) != 48_000
        || le32(&header, 28) != 48_000 * u32::from(media.channels) * 4
        || le16(&header, 32) != media.channels * 4 || le16(&header, 34) != 32
        || u64::from(le32(&header, 40)) != data_len
    {
        return Err(invalid(format!("media {} has a noncanonical float WAV header or changed dimensions", media.id)));
    }
    let mut hash = Sha256::new();
    hash.update(header);
    let mut buf = [0u8; 64 * 1024];
    let mut remaining = data_len;
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buf.len() as u64)).map_err(|_| invalid("media length overflow"))?;
        file.read_exact(&mut buf[..count]).map_err(|e| io(path, e))?;
        hash.update(&buf[..count]);
        if buf[..count].chunks_exact(4).any(|sample| {
            let bits = u32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
            !f32::from_bits(bits).is_finite()
        }) {
            return Err(invalid(format!("media {} contains nonfinite audio", media.id)));
        }
        remaining -= count as u64;
    }
    let mut extra = [0u8; 1];
    if file.read(&mut extra).map_err(|e| io(path, e))? != 0 {
        return Err(invalid(format!("media {} grew while converting", media.id)));
    }
    let actual = format!("{:x}", hash.finalize());
    if actual != media.sha256 {
        return Err(invalid(format!("media {} SHA-256 does not match project.json", media.id)));
    }
    Ok(())
}

fn build_session<'a>(doc: &'a LegacyDocument, root: &Path) -> Result<(Session, Vec<(PathBuf, &'a LegacyMedia)>, usize)> {
    let legacy = &doc.session;
    if legacy.version != 1 || legacy.sample_rate != 48_000 || !(40..=240).contains(&legacy.tempo_bpm) {
        return Err(invalid("requires version 1, 48 kHz, and tempo 40..240 BPM"));
    }
    if legacy.media.len() > 4096 || legacy.tracks.len() > 4096 {
        return Err(invalid("project exceeds bounded source/track count"));
    }
    let media_dir = root.join("media");
    if !legacy.media.is_empty() {
        let canonical = fs::canonicalize(&media_dir).map_err(|e| io(&media_dir, e))?;
        if !canonical.starts_with(root) || !canonical.is_dir() {
            return Err(invalid("media directory resolves outside the source project"));
        }
    }
    let mut seen = HashSet::new();
    let mut sources = HashMap::new();
    let mut paths = Vec::new();
    let mut total_bytes = 0u64;
    let mut max_id = 0u64;
    let name = root.file_stem().and_then(|n| n.to_str()).ok_or_else(|| invalid("source directory name must be UTF-8"))?;
    let mut session = Session::new(name, SampleRate::HZ_48000);
    session.tempo = TempoMap::new(f64::from(legacy.tempo_bpm), 4, 4).map_err(|e| invalid(e.to_string()))?;
    for media in &legacy.media {
        if media.id == 0 || !seen.insert(media.id) || !(1..=2).contains(&media.channels) || media.frames == 0
            || media.sha256.len() != 64 || !media.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(invalid(format!("media {} has invalid id, channels, frames, or SHA-256", media.id)));
        }
        checked_sample(media.frames, "media frames")?;
        max_id = max_id.max(media.id);
        let path = media_dir.join(format!("{}.wav", media.id));
        let size = fs::symlink_metadata(&path).map_err(|e| io(&path, e))?.len();
        total_bytes = total_bytes.checked_add(size).ok_or_else(|| invalid("media size overflow"))?;
        if total_bytes > MAX_TOTAL_MEDIA_BYTES {
            return Err(invalid("combined media exceeds the 256 MiB migration limit"));
        }
        verify_media(&path, media)?;
        let abs = fs::canonicalize(&path).map_err(|e| io(&path, e))?;
        if !abs.starts_with(&media_dir) { return Err(invalid("media resolves outside the source project")); }
        let stored = abs.to_str().ok_or_else(|| invalid("media path must be UTF-8"))?.to_owned();
        session.sources.push(Source {
            id: SourceId(media.id), name: format!("Media {}", media.id), path: stored,
            channels: media.channels, frames: media.frames, sample_rate: 48_000,
            format: FileFormat::Wav, time_reference: 0, unsaved: false,
        });
        paths.push((abs, media));
        sources.insert(media.id, media);
    }
    let mut clip_count = 0usize;
    for legacy_track in &legacy.tracks {
        if legacy_track.id == 0 || !seen.insert(legacy_track.id) || legacy_track.name.is_empty()
            || !legacy_track.pan.is_finite() || !(-1.0..=1.0).contains(&legacy_track.pan) {
            return Err(invalid(format!("track {} has invalid id, name, or pan", legacy_track.id)));
        }
        max_id = max_id.max(legacy_track.id);
        clip_count = clip_count.checked_add(legacy_track.clips.len()).ok_or_else(|| invalid("clip count overflow"))?;
        if clip_count > 100_000 { return Err(invalid("project exceeds 100,000 clips")); }
        let mut channels = None;
        for clip in &legacy_track.clips {
            let media = sources.get(&clip.media_id).ok_or_else(|| invalid(format!("clip {} refers to absent media {}", clip.id, clip.media_id)))?;
            if channels.replace(media.channels).is_some_and(|old| old != media.channels) {
                return Err(invalid(format!("track {} mixes mono and stereo media", legacy_track.id)));
            }
        }
        let format = if channels == Some(2) { ChannelFormat::Stereo } else { ChannelFormat::Mono };
        if format == ChannelFormat::Stereo && legacy_track.pan != 0.0 {
            return Err(invalid(format!("stereo track {} has pan {}; stereo balance differs in the new mixer", legacy_track.id, legacy_track.pan)));
        }
        let mut track = Track::new(TrackId(legacy_track.id), legacy_track.name.clone(), TrackKind::Audio, format, [80, 145, 190]);
        track.mixer.volume_db = gain_db(legacy_track.gain, &format!("track {} gain", legacy_track.id))?;
        track.mixer.mute = legacy_track.mute;
        track.mixer.solo = legacy_track.solo;
        if format == ChannelFormat::Mono { track.mixer.pan = vec![legacy_track.pan]; }
        let mut intervals = Vec::new();
        for clip in &legacy_track.clips {
            let media = sources.get(&clip.media_id).ok_or_else(|| invalid("missing clip media"))?;
            if clip.id == 0 || !seen.insert(clip.id) || clip.len == 0 || clip.fade_in > clip.len || clip.fade_out > clip.len
                || clip.fade_out != 0 {
                return Err(invalid(format!("clip {} has invalid id/length/fade or unsupported fade-out", clip.id)));
            }
            let source_end = clip.source_in.checked_add(clip.len).ok_or_else(|| invalid("clip source position overflow"))?;
            let timeline_end = clip.start.checked_add(clip.len).ok_or_else(|| invalid("clip timeline position overflow"))?;
            if source_end > media.frames { return Err(invalid(format!("clip {} exceeds source {}", clip.id, clip.media_id))); }
            intervals.push((clip.start, timeline_end));
            max_id = max_id.max(clip.id);
            let mut new_clip = Clip::audio(ClipId(clip.id), format!("Media {}", clip.media_id), SourceId(clip.media_id),
                checked_sample(clip.source_in, "source-in")?, checked_sample(clip.start, "clip start")?, checked_sample(clip.len, "clip length")?);
            checked_sample(timeline_end, "clip end")?;
            new_clip.gain_db = gain_db(clip.gain, &format!("clip {} gain", clip.id))?;
            new_clip.fade_in = Fade { len: checked_sample(clip.fade_in, "fade-in")?, shape: FadeShape::Linear };
            track.playlist_mut().ok_or_else(|| invalid("new track has no playlist"))?.clips.push(new_clip);
        }
        intervals.sort_unstable();
        if intervals.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(invalid(format!("track {} has overlapping clips; overlap mixing is not fidelity-verified", legacy_track.id)));
        }
        session.tracks.push(track);
    }
    if legacy.next_id <= max_id {
        return Err(invalid("next_id does not exceed every existing id"));
    }
    session.next_id = legacy.next_id;
    session.comments = format!(
        "Migrated from legacy AudioStrike directory {}. Original media are referenced in place; keep that directory and media files unchanged. Legacy undo/redo history was {}.",
        root.display(), if doc.undo.is_empty() && doc.redo.is_empty() { "empty" } else { "omitted by explicit current-state-only choice; history remains in the original project" }
    );
    Ok((session, paths, clip_count))
}

/// Create a new `.scraft` file outside the original project tree, without replacing
/// any destination. This is structural migration, not a sample-identical mix claim.
pub fn convert_legacy(source: &Path, output: &Path, current_state_only: bool) -> Result<ConversionReport> {
    if source.extension().and_then(|x| x.to_str()) != Some("audiostrike")
        || output.extension().and_then(|x| x.to_str()) != Some("scraft") {
        return Err(invalid("expected SOURCE.audiostrike and OUTPUT.scraft"));
    }
    let root = fs::canonicalize(source).map_err(|e| io(source, e))?;
    if !root.is_dir() { return Err(invalid("source must be a project directory")); }
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|e| io(parent, e))?;
    if parent.starts_with(&root) { return Err(invalid("output must be outside the original project tree")); }
    let (doc, original_manifest) = read_document(&root)?;
    if (!doc.undo.is_empty() || !doc.redo.is_empty()) && !current_state_only {
        return Err(invalid(format!("project has {} undo and {} redo snapshots; pass --current-state-only to omit that history while preserving the original project", doc.undo.len(), doc.redo.len())));
    }
    let (session, paths, clips) = build_session(&doc, &root)?;
    let text = session.to_json().map_err(|e| invalid(format!("new session serialization: {e}")))?;
    // Detect source changes between first validation and publication. Later edits to
    // the legacy tree cannot be prevented; the converted session explicitly depends on it.
    for (path, media) in paths { verify_media(&path, media)?; }
    let manifest_path = root.join("project.json");
    let final_manifest = File::open(&manifest_path).map_err(|e| io(&manifest_path, e))?
        .take(MAX_DOCUMENT_BYTES + 1).bytes().collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| io(&manifest_path, e))?;
    if final_manifest != original_manifest {
        return Err(invalid("project.json changed while converting; retry after closing the legacy editor"));
    }
    publish_new_export(output, text.as_bytes(), None)?;
    Ok(ConversionReport {
        sources: session.sources.len(), tracks: session.tracks.len(), clips,
        omitted_undo: doc.undo.len(), omitted_redo: doc.redo.len(), source_root: root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        parent: PathBuf,
        project: PathBuf,
        output: PathBuf,
        manifest: PathBuf,
        media: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let parent = std::env::temp_dir().join(format!(
                "audiostrike-legacy-test-{}-{}",
                std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let project = parent.join("example.audiostrike");
            let media_dir = project.join("media");
            fs::create_dir_all(&media_dir).expect("private fixture directory");
            let media = media_dir.join("1.wav");
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&44u32.to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&3u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&48_000u32.to_le_bytes());
            wav.extend_from_slice(&192_000u32.to_le_bytes());
            wav.extend_from_slice(&4u16.to_le_bytes());
            wav.extend_from_slice(&32u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&8u32.to_le_bytes());
            wav.extend_from_slice(&0.25f32.to_le_bytes());
            wav.extend_from_slice(&(-0.25f32).to_le_bytes());
            fs::write(&media, &wav).expect("fixture WAV");
            let hash = format!("{:x}", Sha256::digest(&wav));
            let manifest = project.join("project.json");
            let doc = json!({
                "session": {"version":1,"sample_rate":48000,"tempo_bpm":120,"next_id":4,
                    "media":[{"id":1,"frames":2,"channels":1,"sha256":hash}],
                    "tracks":[{"id":2,"name":"Guide","gain":1.0,"pan":0.0,"mute":false,"solo":false,
                        "clips":[{"id":3,"media_id":1,"source_in":0,"len":2,"start":0,"gain":1.0,"fade_in":0,"fade_out":0}]}]},
                "undo":[],"redo":[]
            });
            fs::write(&manifest, serde_json::to_vec(&doc).expect("fixture JSON")).expect("fixture manifest");
            let output = parent.join("converted.scraft");
            Self { parent, project, output, manifest, media }
        }

        fn edit(&self, change: impl FnOnce(&mut Value)) {
            let bytes = fs::read(&self.manifest).expect("manifest");
            let mut doc: Value = serde_json::from_slice(&bytes).expect("fixture JSON");
            change(&mut doc);
            fs::write(&self.manifest, serde_json::to_vec(&doc).expect("fixture JSON")).expect("manifest");
        }

        fn convert(&self, current_state_only: bool) -> Result<ConversionReport> {
            convert_legacy(&self.project, &self.output, current_state_only)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.parent.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("audiostrike-legacy-test-")) {
                let _ = fs::remove_dir_all(&self.parent);
            }
        }
    }

    #[test]
    fn converts_editable_session_and_preserves_original_bytes() {
        let fixture = Fixture::new();
        let before_manifest = fs::read(&fixture.manifest).expect("manifest");
        let before_media = fs::read(&fixture.media).expect("media");
        let report = fixture.convert(false).expect("convert");
        assert_eq!((report.sources, report.tracks, report.clips), (1, 1, 1));
        let converted = fs::read_to_string(&fixture.output).expect("converted session");
        let session = Session::from_json(&converted).expect("reopen model");
        assert_eq!(session.tracks[0].clips()[0].id, ClipId(3));
        assert_eq!(session.sources[0].id, SourceId(1));
        assert_eq!(session.next_id, 4);
        assert!(session.sources[0].path.contains("example.audiostrike"));
        assert_eq!(fs::read(&fixture.manifest).expect("manifest"), before_manifest);
        assert_eq!(fs::read(&fixture.media).expect("media"), before_media);
    }

    #[test]
    fn history_needs_explicit_current_state_choice() {
        let fixture = Fixture::new();
        fixture.edit(|doc| {
            let snapshot = doc["session"].clone();
            doc["undo"] = json!([snapshot]);
        });
        assert!(fixture.convert(false).is_err());
        assert!(!fixture.output.exists());
        let report = fixture.convert(true).expect("explicit current-state conversion");
        assert_eq!(report.omitted_undo, 1);
        let session = Session::from_json(&fs::read_to_string(&fixture.output).expect("output")).expect("session");
        assert!(session.comments.contains("omitted by explicit current-state-only choice"));
    }

    #[test]
    fn rejects_unsupported_fade_overlap_and_unknown_schema() {
        let fixture = Fixture::new();
        fixture.edit(|doc| doc["session"]["tracks"][0]["clips"][0]["fade_out"] = json!(1));
        assert!(fixture.convert(false).is_err());
        assert!(!fixture.output.exists());
        fixture.edit(|doc| {
            doc["session"]["tracks"][0]["clips"][0]["fade_out"] = json!(0);
            doc["session"]["unexpected"] = json!(true);
        });
        assert!(fixture.convert(false).is_err());
        assert!(!fixture.output.exists());
    }

    #[test]
    fn rejects_tampered_media_and_recovery_sidecar() {
        let fixture = Fixture::new();
        let mut bytes = fs::read(&fixture.media).expect("media");
        bytes[44] ^= 1;
        fs::write(&fixture.media, bytes).expect("tamper");
        assert!(fixture.convert(false).is_err());
        assert!(!fixture.output.exists());
        fs::write(fixture.project.join("project.json.tmp"), b"sentinel").expect("sidecar");
        assert!(fixture.convert(false).is_err());
        assert_eq!(fs::read(fixture.project.join("project.json.tmp")).expect("sidecar"), b"sentinel");
    }

    #[test]
    fn refuses_occupied_output_without_mutating_it() {
        let fixture = Fixture::new();
        fs::write(&fixture.output, b"occupied sentinel").expect("output sentinel");
        assert!(fixture.convert(false).is_err());
        assert_eq!(fs::read(&fixture.output).expect("output"), b"occupied sentinel");
    }

    #[test]
    fn retains_existing_legacy_backup() {
        let fixture = Fixture::new();
        let backup = fixture.project.join("project.json.bak");
        fs::write(&backup, b"old recovery snapshot").expect("backup");
        fixture.convert(false).expect("convert current manifest");
        assert_eq!(fs::read(&backup).expect("backup"), b"old recovery snapshot");
    }

    #[test]
    fn refuses_output_in_original_project_tree() {
        let fixture = Fixture::new();
        let output = fixture.project.join("new.scraft");
        assert!(convert_legacy(&fixture.project, &output, false).is_err());
        assert!(!output.exists());
    }
}
