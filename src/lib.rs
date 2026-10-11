//! AudioStrike's portable, local multitrack document and offline mix core.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "desktop")]
pub mod device;
pub mod donor;
pub mod stream;
#[path = "../third_party/filmcraft/resample.rs"]
mod filmcraft_resample;

const RATE: u32 = 48_000;
const MAX_WAV_BYTES: u64 = 256 * 1024 * 1024;
static NEXT_IMPORT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Json(e) => write!(f, "project JSON: {e}"),
            Self::Invalid(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: u64,
    pub frames: u64,
    pub channels: u16,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub media_id: u64,
    pub source_in: u64,
    pub len: u64,
    pub start: u64,
    pub gain: f32,
    pub fade_in: u64,
    pub fade_out: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub gain: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub clips: Vec<Clip>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub sample_rate: u32,
    pub tempo_bpm: u16,
    pub next_id: u64,
    pub media: Vec<MediaAsset>,
    pub tracks: Vec<Track>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Document {
    session: Session,
    undo: Vec<Session>,
    redo: Vec<Session>,
}

struct StagedMedia {
    path: PathBuf,
}
impl StagedMedia {
    fn create(dir: &Path, id: u64) -> Result<(Self, fs::File)> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("system clock is before Unix epoch".into()))?.as_nanos();
        for _ in 0..32 {
            let sequence = NEXT_IMPORT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".{id}.audiostrike-import-{}-{nonce}-{sequence}.tmp", std::process::id()));
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::Invalid("could not reserve a unique media staging file".into()))
    }
}
impl Drop for StagedMedia {
    fn drop(&mut self) { let _ = fs::remove_file(&self.path); }
}

// hard_link publishes complete bytes atomically on supporting filesystems. The
// create_new fallback also refuses existing names on filesystems without links.
fn publish_new_file(source: &Path, destination: &Path) -> Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            Err(Error::Invalid(format!("destination already exists: {}", destination.display())))
        }
        Err(_) => {
            let mut input = fs::File::open(source)?;
            let mut output = fs::OpenOptions::new().write(true).create_new(true).open(destination)?;
            let result = io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
            drop(output);
            if let Err(e) = result {
                let _ = fs::remove_file(destination);
                return Err(e.into());
            }
            let verified = hash_file(source).and_then(|expected| {
                hash_file(destination).map(|actual| expected == actual)
            });
            match verified {
                Ok(true) => {}
                Ok(false) => {
                    let _ = fs::remove_file(destination);
                    return Err(Error::Invalid("published file differs from staged file".into()));
                }
                Err(e) => {
                    let _ = fs::remove_file(destination);
                    return Err(e);
                }
            }
            Ok(())
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn optional_hash_file(path: &Path) -> Result<Option<String>> {
    match fs::File::open(path) {
        Ok(mut file) => {
            let mut hash = Sha256::new();
            io::copy(&mut file, &mut hash)?;
            Ok(Some(format!("{:x}", hash.finalize())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub struct Project {
    root: PathBuf,
    doc: Document,
    manifest_hash: Option<String>,
}

impl Project {
    pub fn create(root: &Path, tempo_bpm: u16) -> Result<Self> {
        if !(40..=240).contains(&tempo_bpm) {
            return Err(Error::Invalid("tempo must be 40..240 BPM".into()));
        }
        if root.extension().and_then(|x| x.to_str()) != Some("audiostrike") {
            return Err(Error::Invalid(
                "project directory must end in .audiostrike".into(),
            ));
        }
        if root.exists() {
            return Err(Error::Invalid("project path already exists".into()));
        }
        fs::create_dir_all(root.join("media"))?;
        let mut project = Self {
            root: root.to_path_buf(),
            manifest_hash: None,
            doc: Document {
                session: Session {
                    version: 1,
                    sample_rate: RATE,
                    tempo_bpm,
                    next_id: 1,
                    media: Vec::new(),
                    tracks: Vec::new(),
                },
                undo: Vec::new(),
                redo: Vec::new(),
            },
        };
        project.save()?;
        Ok(project)
    }
    pub fn open(root: &Path) -> Result<Self> {
        let primary = root.join("project.json");
        let backup = root.join("project.json.bak");
        let path = if primary.exists() { primary } else { backup };
        let bytes = fs::read(&path)?;
        let doc: Document = serde_json::from_slice(&bytes)?;
        validate_document(&doc)?;
        Ok(Self {
            root: root.to_path_buf(),
            doc,
            manifest_hash: (path == root.join("project.json")).then(|| hash_bytes(&bytes)),
        })
    }
    pub fn session(&self) -> &Session {
        &self.doc.session
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn new_id(&mut self) -> Result<u64> {
        let id = self.doc.session.next_id;
        self.doc.session.next_id = id.checked_add(1)
            .ok_or_else(|| Error::Invalid("project ID space exhausted".into()))?;
        Ok(id)
    }
    fn checkpoint_undo(&mut self, before: Session) {
        self.doc.undo.push(before);
        if self.doc.undo.len() > 100 {
            self.doc.undo.remove(0);
        }
        self.doc.redo.clear();
    }
    pub fn save(&mut self) -> Result<()> {
        validate_document(&self.doc)?;
        let bytes = serde_json::to_vec_pretty(&self.doc)?;
        let tmp = self.root.join("project.json.tmp");
        let main = self.root.join("project.json");
        let bak = self.root.join("project.json.bak");
        if tmp.exists() || bak.exists() {
            return Err(Error::Invalid("recover existing project temp or backup before saving".into()));
        }
        if optional_hash_file(&main)? != self.manifest_hash {
            return Err(Error::Invalid("project.json changed on disk; reload or save a copy before overwriting".into()));
        }
        {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            if let Err(e) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
        }
        let disk_hash = match optional_hash_file(&main) {
            Ok(hash) => hash,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if disk_hash != self.manifest_hash {
            let _ = fs::remove_file(&tmp);
            return Err(Error::Invalid("project.json changed during save; existing work preserved".into()));
        }
        if self.manifest_hash.is_some() {
            if let Err(e) = fs::rename(&main, &bak) {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
            let backup_hash = optional_hash_file(&bak);
            if !matches!(&backup_hash, Ok(hash) if hash == &self.manifest_hash) {
                if !main.exists() { let _ = fs::rename(&bak, &main); }
                let _ = fs::remove_file(&tmp);
                return Err(Error::Invalid("project.json changed during save; existing work preserved".into()));
            }
        }
        if let Err(e) = publish_new_file(&tmp, &main) {
            if bak.exists() {
                if !main.exists() { let _ = fs::rename(&bak, &main); }
            }
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.manifest_hash = Some(hash_bytes(&bytes));
        fs::remove_file(&tmp)?;
        if bak.exists() {
            fs::remove_file(bak)?;
        }
        Ok(())
    }
    pub fn offline_media(&self) -> Vec<u64> {
        self.doc
            .session
            .media
            .iter()
            .filter(|m| !self.media_path(m.id).is_file())
            .map(|m| m.id)
            .collect()
    }
    fn media_path(&self, id: u64) -> PathBuf {
        self.root.join("media").join(format!("{id}.wav"))
    }
    pub fn import_wav(&mut self, input: &Path) -> Result<u64> {
        validate_session(&self.doc.session)?;
        let original = read_wav(input)?;
        if ![44_100, RATE].contains(&original.sample_rate) {
            return Err(Error::Invalid("WAV must be 44.1 or 48 kHz".into()));
        }
        if original.frames() as u64 > original.sample_rate as u64 * 2 * 60 * 60 {
            return Err(Error::Invalid("WAV exceeds 2 hours".into()));
        }
        let canonical = if original.sample_rate == RATE {
            original
        } else {
            resample_to_48k(&original)?
        };
        let id = self.doc.session.next_id;
        id.checked_add(1).ok_or_else(|| Error::Invalid("project ID space exhausted".into()))?;
        let final_path = self.media_path(id);
        let (staged, mut file) = StagedMedia::create(&self.root.join("media"), id)?;
        write_wav_float_to(&mut file, &canonical)?;
        drop(file);
        let hash = hash_file(&staged.path)?;
        publish_new_file(&staged.path, &final_path)?;
        let before = self.doc.session.clone();
        self.new_id()?;
        self.doc.session.media.push(MediaAsset {
            id,
            frames: canonical.frames() as u64,
            channels: canonical.channels,
            sha256: hash,
        });
        self.checkpoint_undo(before);
        Ok(id)
    }
    pub fn add_track(&mut self, name: &str) -> Result<u64> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(Error::Invalid(
                "track name must contain 1..100 characters".into(),
            ));
        }
        let before = self.doc.session.clone();
        let id = self.new_id()?;
        self.doc.session.tracks.push(Track {
            id,
            name: name.into(),
            gain: 1.0,
            pan: 0.0,
            mute: false,
            solo: false,
            clips: Vec::new(),
        });
        self.checkpoint_undo(before);
        Ok(id)
    }
    pub fn place_clip(&mut self, track_id: u64, media_id: u64, start: u64) -> Result<u64> {
        let asset = self
            .doc
            .session
            .media
            .iter()
            .find(|m| m.id == media_id)
            .ok_or_else(|| Error::Invalid("media ID not found".into()))?;
        let len = asset.frames;
        start
            .checked_add(len)
            .ok_or_else(|| Error::Invalid("clip position overflows".into()))?;
        let index = self
            .doc
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| Error::Invalid("track ID not found".into()))?;
        let before = self.doc.session.clone();
        let id = self.new_id()?;
        self.doc.session.tracks[index].clips.push(Clip {
            id,
            media_id,
            source_in: 0,
            len,
            start,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
        });
        self.checkpoint_undo(before);
        Ok(id)
    }
    pub fn move_clip(&mut self, clip_id: u64, start: u64) -> Result<()> {
        let (ti, ci) = self.find_clip(clip_id)?;
        start
            .checked_add(self.doc.session.tracks[ti].clips[ci].len)
            .ok_or_else(|| Error::Invalid("clip position overflows".into()))?;
        let before = self.doc.session.clone();
        self.doc.session.tracks[ti].clips[ci].start = start;
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn split_clip(&mut self, clip_id: u64, frame: u64) -> Result<u64> {
        let (ti, ci) = self.find_clip(clip_id)?;
        let old = self.doc.session.tracks[ti].clips[ci].clone();
        if frame <= old.start || frame >= old.start + old.len {
            return Err(Error::Invalid("split must be inside clip".into()));
        }
        let before = self.doc.session.clone();
        let id = self.new_id()?;
        let left_len = frame - old.start;
        let mut right = self.doc.session.tracks[ti].clips[ci].clone();
        right.id = id;
        right.start = frame;
        right.source_in += left_len;
        right.len -= left_len;
        right.fade_in = 0;
        self.doc.session.tracks[ti].clips[ci].len = left_len;
        self.doc.session.tracks[ti].clips[ci].fade_out = 0;
        self.doc.session.tracks[ti].clips.insert(ci + 1, right);
        self.checkpoint_undo(before);
        Ok(id)
    }
    fn find_clip(&self, id: u64) -> Result<(usize, usize)> {
        self.doc
            .session
            .tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.clips.iter().position(|c| c.id == id).map(|ci| (ti, ci)))
            .ok_or_else(|| Error::Invalid("clip ID not found".into()))
    }
    pub fn undo(&mut self) -> Result<()> {
        let previous = self
            .doc
            .undo
            .pop()
            .ok_or_else(|| Error::Invalid("nothing to undo".into()))?;
        self.doc
            .redo
            .push(std::mem::replace(&mut self.doc.session, previous));
        Ok(())
    }
    pub fn redo(&mut self) -> Result<()> {
        let next = self
            .doc
            .redo
            .pop()
            .ok_or_else(|| Error::Invalid("nothing to redo".into()))?;
        self.doc
            .undo
            .push(std::mem::replace(&mut self.doc.session, next));
        Ok(())
    }
    pub fn set_track_gain_db(&mut self, track_id: u64, db: f64) -> Result<()> {
        if !db.is_finite() || !(-96.0..=15.0).contains(&db) {
            return Err(Error::Invalid("track gain must be -96..15 dB".into()));
        }
        let index = self
            .doc
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| Error::Invalid("track ID not found".into()))?;
        let before = self.doc.session.clone();
        self.doc.session.tracks[index].gain = 10.0_f32.powf(db as f32 / 20.0);
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn set_track_pan(&mut self, track_id: u64, pan: f32) -> Result<()> {
        if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
            return Err(Error::Invalid("track pan must be -1..1".into()));
        }
        let index = self
            .doc
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| Error::Invalid("track ID not found".into()))?;
        let before = self.doc.session.clone();
        self.doc.session.tracks[index].pan = pan;
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn set_track_mute(&mut self, track_id: u64, mute: bool) -> Result<()> {
        let index = self
            .doc
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| Error::Invalid("track ID not found".into()))?;
        let before = self.doc.session.clone();
        self.doc.session.tracks[index].mute = mute;
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn set_track_solo(&mut self, track_id: u64, solo: bool) -> Result<()> {
        let index = self
            .doc
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| Error::Invalid("track ID not found".into()))?;
        let before = self.doc.session.clone();
        self.doc.session.tracks[index].solo = solo;
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn set_clip_fades(&mut self, clip_id: u64, fade_in: u64, fade_out: u64) -> Result<()> {
        let (ti, ci) = self.find_clip(clip_id)?;
        if fade_in
            .checked_add(fade_out)
            .is_none_or(|n| n > self.doc.session.tracks[ti].clips[ci].len)
        {
            return Err(Error::Invalid("fades exceed clip length".into()));
        }
        let before = self.doc.session.clone();
        let clip = &mut self.doc.session.tracks[ti].clips[ci];
        clip.fade_in = fade_in;
        clip.fade_out = fade_out;
        self.checkpoint_undo(before);
        Ok(())
    }
    pub fn render_samples(&self) -> Result<Vec<f32>> {
        let offline = self.offline_media();
        if !offline.is_empty() {
            return Err(Error::Invalid(format!("offline media IDs: {offline:?}")));
        }
        let frames = self
            .doc
            .session
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .filter_map(|c| c.start.checked_add(c.len))
            .max()
            .unwrap_or(0);
        if frames > RATE as u64 * 60 * 60 {
            return Err(Error::Invalid(
                "first build limits renders to one hour".into(),
            ));
        }
        let mut out = vec![0.0_f32; frames as usize * 2];
        let any_solo = self.doc.session.tracks.iter().any(|t| t.solo);
        for track in &self.doc.session.tracks {
            if track.mute || (any_solo && !track.solo) {
                continue;
            }
            for clip in &track.clips {
                let asset = self
                    .doc
                    .session
                    .media
                    .iter()
                    .find(|m| m.id == clip.media_id)
                    .ok_or_else(|| Error::Invalid("clip refers to missing media record".into()))?;
                let path = self.media_path(asset.id);
                if hash_file(&path)? != asset.sha256 {
                    return Err(Error::Invalid(format!(
                        "media {} changed on disk",
                        asset.id
                    )));
                }
                let source = read_wav(&path)?;
                if source.frames() as u64 != asset.frames {
                    return Err(Error::Invalid("media length changed".into()));
                }
                let pan = track.pan.clamp(-1.0, 1.0);
                let left_gain =
                    ((pan + 1.0) * std::f32::consts::FRAC_PI_4).cos() * track.gain * clip.gain;
                let right_gain =
                    ((pan + 1.0) * std::f32::consts::FRAC_PI_4).sin() * track.gain * clip.gain;
                for i in 0..clip.len as usize {
                    let dst = (clip.start as usize + i) * 2;
                    let src = (clip.source_in as usize + i) * source.channels as usize;
                    let fade_in = if clip.fade_in > 0 {
                        (i as f32 / clip.fade_in as f32).min(1.0)
                    } else {
                        1.0
                    };
                    let remaining = clip.len as usize - 1 - i;
                    let fade_out = if clip.fade_out > 0 {
                        (remaining as f32 / clip.fade_out as f32).min(1.0)
                    } else {
                        1.0
                    };
                    let fade = fade_in.min(fade_out);
                    if source.channels == 1 {
                        out[dst] += source.samples[src] * left_gain * fade;
                        out[dst + 1] += source.samples[src] * right_gain * fade;
                    } else {
                        // At centre, stereo input keeps unity gain on each channel.
                        out[dst] +=
                            source.samples[src] * left_gain * std::f32::consts::SQRT_2 * fade;
                        out[dst + 1] +=
                            source.samples[src + 1] * right_gain * std::f32::consts::SQRT_2 * fade;
                    }
                }
            }
        }
        if out.iter().any(|x| !x.is_finite() || x.abs() > 1.0) {
            return Err(Error::Invalid(
                "mix exceeds 0 dBFS; reduce gain before export".into(),
            ));
        }
        Ok(out)
    }
    pub fn render_wav(&self, destination: &Path) -> Result<()> {
        self.render_wav_streaming(destination)
    }
}

fn validate_document(doc: &Document) -> Result<()> {
    validate_session(&doc.session)?;
    for session in doc.undo.iter().chain(&doc.redo) {
        validate_session(session)?;
    }
    Ok(())
}

fn validate_session(session: &Session) -> Result<()> {
    if session.version != 1
        || session.sample_rate != RATE
        || !(40..=240).contains(&session.tempo_bpm)
    {
        return Err(Error::Invalid(
            "unsupported project schema, rate, or tempo".into(),
        ));
    }
    let mut ids = HashSet::new();
    let mut largest_id = 0;
    for media in &session.media {
        if media.id == 0 || !ids.insert(media.id) {
            return Err(Error::Invalid("duplicate or zero project ID".into()));
        }
        largest_id = largest_id.max(media.id);
        if !(1..=2).contains(&media.channels)
            || media.frames.checked_mul(media.channels as u64)
                .and_then(|n| n.checked_mul(4))
                .is_none_or(|n| n > u32::MAX as u64)
        {
            return Err(Error::Invalid("invalid media frame count or channels".into()));
        }
    }
    for track in &session.tracks {
        if track.id == 0 || !ids.insert(track.id) {
            return Err(Error::Invalid("duplicate or zero project ID".into()));
        }
        largest_id = largest_id.max(track.id);
        if !track.gain.is_finite()
            || !(0.0..=5.624).contains(&track.gain)
            || !track.pan.is_finite()
            || !(-1.0..=1.0).contains(&track.pan)
        {
            return Err(Error::Invalid("invalid track controls".into()));
        }
        for clip in &track.clips {
            if clip.id == 0 || !ids.insert(clip.id) {
                return Err(Error::Invalid("duplicate or zero project ID".into()));
            }
            largest_id = largest_id.max(clip.id);
            let asset = session
                .media
                .iter()
                .find(|m| m.id == clip.media_id)
                .ok_or_else(|| Error::Invalid("unknown clip media".into()))?;
            if clip
                .source_in
                .checked_add(clip.len)
                .is_none_or(|x| x > asset.frames)
                || clip.start.checked_add(clip.len).is_none()
                || !clip.gain.is_finite()
                || !(0.0..=4.0).contains(&clip.gain)
            {
                return Err(Error::Invalid("invalid clip bounds or gain".into()));
            }
        }
    }
    if session.next_id <= largest_id {
        return Err(Error::Invalid("next_id collides with project IDs".into()));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}
impl WavData {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }
}

pub fn read_wav(path: &Path) -> Result<WavData> {
    if fs::metadata(path)?.len() > MAX_WAV_BYTES {
        return Err(Error::Invalid("WAV exceeds 256 MiB import limit".into()));
    }
    let bytes = fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Error::Invalid("invalid RIFF/WAVE header".into()));
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8;
    if declared != bytes.len() {
        return Err(Error::Invalid("RIFF length mismatch".into()));
    }
    let mut format = None;
    let mut data = None;
    let mut cursor = 12_usize;
    while cursor < bytes.len() {
        if cursor + 8 > bytes.len() {
            return Err(Error::Invalid("truncated WAV chunk".into()));
        }
        let id = &bytes[cursor..cursor + 4];
        let len = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let begin = cursor + 8;
        let end = begin
            .checked_add(len)
            .ok_or_else(|| Error::Invalid("WAV chunk length overflow".into()))?;
        if end > bytes.len() {
            return Err(Error::Invalid("truncated WAV data".into()));
        }
        if id == b"fmt " {
            format = Some(&bytes[begin..end]);
        }
        if id == b"data" {
            data = Some(&bytes[begin..end]);
        }
        cursor = end + (len % 2);
    }
    let fmt = format.ok_or_else(|| Error::Invalid("WAV format chunk missing".into()))?;
    let data = data.ok_or_else(|| Error::Invalid("WAV data chunk missing".into()))?;
    if fmt.len() < 16 {
        return Err(Error::Invalid("short WAV format chunk".into()));
    }
    let code = u16::from_le_bytes(fmt[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
    if !(1..=2).contains(&channels) {
        return Err(Error::Invalid("only mono/stereo WAV is supported".into()));
    }
    if ![44_100, RATE].contains(&sample_rate) {
        return Err(Error::Invalid("only 44.1/48 kHz WAV is supported".into()));
    }
    let bytes_per_sample = match (code, bits) {
        (1, 16) => 2,
        (1, 24) => 3,
        (3, 32) => 4,
        _ => {
            return Err(Error::Invalid(
                "only PCM 16/24-bit or float 32-bit WAV is supported".into(),
            ));
        }
    };
    let block = bytes_per_sample * channels as usize;
    if u16::from_le_bytes(fmt[12..14].try_into().unwrap()) as usize != block
        || data.len() % block != 0
    {
        return Err(Error::Invalid("WAV block alignment mismatch".into()));
    }
    let mut samples = Vec::with_capacity(data.len() / bytes_per_sample);
    for chunk in data.chunks_exact(bytes_per_sample) {
        let value = match (code, bits) {
            (1, 16) => i16::from_le_bytes(chunk.try_into().unwrap()) as f32 / 32768.0,
            (1, 24) => {
                let raw =
                    (chunk[0] as i32) | ((chunk[1] as i32) << 8) | ((chunk[2] as i8 as i32) << 16);
                raw as f32 / 8_388_608.0
            }
            (3, 32) => f32::from_le_bytes(chunk.try_into().unwrap()),
            _ => unreachable!(),
        };
        if !value.is_finite() {
            return Err(Error::Invalid("non-finite WAV sample".into()));
        }
        samples.push(value);
    }
    Ok(WavData {
        sample_rate,
        channels,
        samples,
    })
}

fn wav_header(file: &mut fs::File, data: &WavData, format: u16, bits: u16) -> Result<()> {
    if data.channels == 0
        || data.samples.len() % data.channels as usize != 0
        || data.samples.iter().any(|x| !x.is_finite())
    {
        return Err(Error::Invalid("invalid output samples".into()));
    }
    let byte_len = data
        .samples
        .len()
        .checked_mul((bits / 8) as usize)
        .ok_or_else(|| Error::Invalid("WAV size overflow".into()))?;
    let byte_len: u32 = byte_len
        .try_into()
        .map_err(|_| Error::Invalid("WAV exceeds RIFF size".into()))?;
    file.write_all(b"RIFF")?;
    file.write_all(&(byte_len + 36).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&format.to_le_bytes())?;
    file.write_all(&data.channels.to_le_bytes())?;
    file.write_all(&data.sample_rate.to_le_bytes())?;
    let block = data.channels * bits / 8;
    file.write_all(&(data.sample_rate * block as u32).to_le_bytes())?;
    file.write_all(&block.to_le_bytes())?;
    file.write_all(&bits.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&byte_len.to_le_bytes())?;
    Ok(())
}
pub fn write_wav_float(path: &Path, data: &WavData) -> Result<()> {
    let mut file = fs::File::create(path)?;
    write_wav_float_to(&mut file, data)
}
fn write_wav_float_to(file: &mut fs::File, data: &WavData) -> Result<()> {
    wav_header(file, data, 3, 32)?;
    for sample in &data.samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.sync_all()?;
    Ok(())
}
fn resample_to_48k(input: &WavData) -> Result<WavData> {
    let output_frames = ((input.frames() as u64 * RATE as u64 + input.sample_rate as u64 / 2)
        / input.sample_rate as u64) as usize;
    let mut resampler = filmcraft_resample::StreamResampler::new(input.sample_rate, RATE);
    let mut samples = Vec::with_capacity(output_frames * input.channels as usize);
    let channels = input.channels as usize;
    let mut pos = 0_usize;
    while pos < output_frames {
        let n = 4096.min(output_frames - pos);
        let planar = resampler.process(pos as i64, n, |start, count| {
            (0..channels)
                .map(|c| {
                    (0..count)
                        .map(|i| {
                            let f = start + i as i64;
                            if f < 0 || f >= input.frames() as i64 {
                                0.0
                            } else {
                                input.samples[f as usize * channels + c]
                            }
                        })
                        .collect()
                })
                .collect()
        });
        for i in 0..n {
            for channel in &planar {
                samples.push(channel[i]);
            }
        }
        pos += n;
    }
    Ok(WavData {
        sample_rate: RATE,
        channels: input.channels,
        samples,
    })
}
