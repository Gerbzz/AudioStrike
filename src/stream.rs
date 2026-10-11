//! Bounded, disk-backed 48 kHz mixer and synthetic-input recording writer.
//! Neither API opens a microphone or other capture device.

use crate::{Error, Project, RATE, Result, Track, hash_file};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const BLOCK_FRAMES: usize = 4096;
const MAX_FRAMES: u64 = RATE as u64 * 60 * 60;
static NEXT_EXPORT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct MediaReader {
    file: File,
    channels: usize,
    frames: u64,
}
impl MediaReader {
    fn open(path: &Path, expected_frames: u64, expected_channels: u16) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0_u8; 44];
        file.read_exact(&mut header)?;
        let u16_at = |at: usize| u16::from_le_bytes(header[at..at + 2].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let channels = u16_at(22);
        let byte_len = u32_at(40) as u64;
        let expected_data_len = expected_frames.checked_mul(channels as u64)
            .and_then(|n| n.checked_mul(4))
            .filter(|n| *n <= u32::MAX as u64)
            .ok_or_else(|| Error::Invalid("media frame count overflows WAV length".into()))?;
        let expected_file_len = expected_data_len.checked_add(44)
            .ok_or_else(|| Error::Invalid("media file length overflow".into()))?;
        let valid = &header[0..4] == b"RIFF"
            && &header[8..16] == b"WAVEfmt "
            && u32_at(16) == 16
            && u16_at(20) == 3
            && channels == expected_channels
            && u32_at(24) == RATE
            && u16_at(34) == 32
            && &header[36..40] == b"data"
            && byte_len == expected_data_len
            && u32_at(4) as u64 + 8 == expected_file_len
            && file.metadata()?.len() == expected_file_len;
        if !valid {
            return Err(Error::Invalid(format!(
                "canonical media header changed: {}",
                path.display()
            )));
        }
        Ok(Self { file, channels: channels as usize, frames: expected_frames })
    }
    fn read_frames(&mut self, at: u64, frames: usize, bytes: &mut [u8]) -> Result<()> {
        let end = at.checked_add(frames as u64).ok_or_else(|| Error::Invalid("media position overflow".into()))?;
        if end > self.frames { return Err(Error::Invalid("media shortened during render".into())); }
        let count = frames * self.channels * 4;
        self.file.seek(SeekFrom::Start(44 + at * self.channels as u64 * 4))?;
        self.file.read_exact(&mut bytes[..count])?;
        Ok(())
    }
}

pub struct StreamingMixer {
    tracks: Vec<Track>,
    sources: HashMap<u64, MediaReader>,
    scratch: Vec<u8>,
    total_frames: u64,
}
impl StreamingMixer {
    pub fn open(project: &Project) -> Result<Self> {
        let total_frames = project.session().tracks.iter().flat_map(|t| &t.clips)
            .filter_map(|c| c.start.checked_add(c.len)).max().unwrap_or(0);
        if total_frames > MAX_FRAMES {
            return Err(Error::Invalid("first build limits renders to one hour".into()));
        }
        let mut sources = HashMap::new();
        for media in &project.session().media {
            let path = project.root().join("media").join(format!("{}.wav", media.id));
            if hash_file(&path)? != media.sha256 {
                return Err(Error::Invalid(format!("media {} changed on disk", media.id)));
            }
            sources.insert(media.id, MediaReader::open(&path, media.frames, media.channels)?);
        }
        Ok(Self {
            tracks: project.session().tracks.clone(),
            sources,
            scratch: vec![0; BLOCK_FRAMES * 2 * 4],
            total_frames,
        })
    }
    pub fn total_frames(&self) -> u64 { self.total_frames }
    pub fn read_frames(&mut self, at: u64, out: &mut [f32]) -> Result<usize> {
        if out.len() % 2 != 0 || out.len() > BLOCK_FRAMES * 2 {
            return Err(Error::Invalid("stream block must contain at most 4096 stereo frames".into()));
        }
        let count = ((self.total_frames.saturating_sub(at)) as usize).min(out.len() / 2);
        out.fill(0.0);
        if count == 0 { return Ok(0); }
        let end = at + count as u64;
        let any_solo = self.tracks.iter().any(|t| t.solo);
        for track in &self.tracks {
            if track.mute || (any_solo && !track.solo) { continue; }
            let pan = track.pan.clamp(-1.0, 1.0);
            let left_gain = ((pan + 1.0) * std::f32::consts::FRAC_PI_4).cos() * track.gain;
            let right_gain = ((pan + 1.0) * std::f32::consts::FRAC_PI_4).sin() * track.gain;
            for clip in &track.clips {
                let begin = at.max(clip.start);
                let stop = end.min(clip.start + clip.len);
                if begin >= stop { continue; }
                let source = self.sources.get_mut(&clip.media_id)
                    .ok_or_else(|| Error::Invalid("clip refers to missing media".into()))?;
                let frames = (stop - begin) as usize;
                let source_at = clip.source_in + (begin - clip.start);
                source.read_frames(source_at, frames, &mut self.scratch)?;
                for i in 0..frames {
                    let clip_frame = (begin - clip.start) as usize + i;
                    let fade_in = if clip.fade_in > 0 { (clip_frame as f32 / clip.fade_in as f32).min(1.0) } else { 1.0 };
                    let remaining = clip.len as usize - 1 - clip_frame;
                    let fade_out = if clip.fade_out > 0 { (remaining as f32 / clip.fade_out as f32).min(1.0) } else { 1.0 };
                    let fade = fade_in.min(fade_out) * clip.gain;
                    let target = ((begin - at) as usize + i) * 2;
                    let sample = |channel: usize| {
                        let offset = (i * source.channels + channel) * 4;
                        f32::from_le_bytes(self.scratch[offset..offset + 4].try_into().unwrap())
                    };
                    if source.channels == 1 {
                        out[target] += sample(0) * left_gain * fade;
                        out[target + 1] += sample(0) * right_gain * fade;
                    } else {
                        out[target] += sample(0) * left_gain * std::f32::consts::SQRT_2 * fade;
                        out[target + 1] += sample(1) * right_gain * std::f32::consts::SQRT_2 * fade;
                    }
                }
            }
        }
        if out[..count * 2].iter().any(|x| !x.is_finite() || x.abs() > 1.0) {
            return Err(Error::Invalid("mix exceeds 0 dBFS; reduce gain before export".into()));
        }
        Ok(count)
    }
}

fn write_header(file: &mut File, channels: u16, frames: u64, code: u16, bits: u16) -> Result<()> {
    let data_len: u32 = frames.checked_mul(channels as u64).and_then(|n| n.checked_mul((bits / 8) as u64))
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| Error::Invalid("WAV exceeds RIFF size".into()))?;
    let riff_len = data_len.checked_add(36).ok_or_else(|| Error::Invalid("WAV exceeds RIFF size".into()))?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_len.to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&code.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&RATE.to_le_bytes())?;
    let align = channels * (bits / 8);
    file.write_all(&(RATE * align as u32).to_le_bytes())?;
    file.write_all(&align.to_le_bytes())?;
    file.write_all(&bits.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    Ok(())
}

// A project can be opened after its directory is renamed, so check both the
// conventional suffix and the on-disk manifest/media structure.
fn reject_project_tree(parent: &Path) -> Result<()> {
    if parent.ancestors().any(|ancestor| {
        ancestor.extension().is_some_and(|extension| extension == std::ffi::OsStr::new("audiostrike"))
            || (ancestor.join("media").is_dir()
                && (ancestor.join("project.json").exists()
                    || ancestor.join("project.json.bak").exists()))
    }) {
        return Err(Error::Invalid(
            "audio output cannot replace or be created inside a project folder".into()
        ));
    }
    Ok(())
}

// RecordingWriter retains explicit replacement for ordinary external files.
// Bounce never calls this, and even this replacement path excludes project data.
fn replace_recording_with_backup(temp: &Path, destination: &Path, backup: &Path) -> Result<()> {
    reject_project_tree(&fs::canonicalize(destination.parent().unwrap())?)?;
    if backup.exists() { return Err(Error::Invalid(format!("recover existing backup: {}", backup.display()))); }
    if destination.exists() { fs::rename(destination, backup)?; }
    if let Err(e) = fs::rename(temp, destination) {
        if backup.exists() { let _ = fs::rename(backup, destination); }
        return Err(e.into());
    }
    if backup.exists() { fs::remove_file(backup)?; }
    Ok(())
}

// The export API has no replacement mode. Resolve the directory before writing so
// symlink/junction aliases into a project are subject to the same project-tree guard.
// Reject every occupied leaf, including dangling symlinks and hard-link aliases.
fn checked_export_destination(project: &Project, destination: &Path) -> Result<PathBuf> {
    if !destination.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav")) {
        return Err(Error::Invalid("bounce destination must end in .wav".into()));
    }
    let name = destination.file_name()
        .ok_or_else(|| Error::Invalid("bounce destination needs a file name".into()))?;
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)?;
    let project_root = fs::canonicalize(project.root())?;
    if parent.starts_with(&project_root) {
        return Err(Error::Invalid(
            "bounce destination is inside the project; choose a WAV outside the project folder".into()
        ));
    }
    reject_project_tree(&parent)?;
    let resolved = parent.join(name);
    match fs::symlink_metadata(&resolved) {
        Ok(_) => Err(Error::Invalid(format!(
            "bounce destination already exists: {}; choose a new WAV name", resolved.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(error.into()),
    }
}

struct StagedExport { path: PathBuf }
impl StagedExport {
    fn create(parent: &Path) -> Result<(Self, File)> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("system clock is before Unix epoch".into()))?.as_nanos();
        for _ in 0..64 {
            let sequence = NEXT_EXPORT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".audiostrike-export-{}-{nonce}-{sequence}.tmp", std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Invalid("could not reserve a unique bounce staging file".into()))
    }
    fn publish_new(&self, destination: &Path) -> Result<()> {
        match fs::hard_link(&self.path, destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::Invalid(
                "bounce destination became occupied; existing file preserved".into()
            )),
            Err(error) => Err(Error::Invalid(format!(
                "atomic no-clobber WAV publish is unavailable here: {error}"
            ))),
        }
    }
}
impl Drop for StagedExport {
    fn drop(&mut self) { let _ = fs::remove_file(&self.path); }
}

impl Project {
    pub fn render_wav_streaming(&self, destination: &Path) -> Result<()> {
        let destination = checked_export_destination(self, destination)?;
        let mut mixer = StreamingMixer::open(self)?;
        let (staged, mut file) = StagedExport::create(destination.parent().unwrap())?;
        let result = (|| {
            write_header(&mut file, 2, mixer.total_frames(), 1, 24)?;
            let mut block = vec![0.0_f32; BLOCK_FRAMES * 2];
            let mut encoded = Vec::with_capacity(BLOCK_FRAMES * 2 * 3);
            let mut at = 0;
            loop {
                let frames = mixer.read_frames(at, &mut block)?;
                if frames == 0 { break; }
                encoded.clear();
                for sample in &block[..frames * 2] {
                    let scaled = (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                    encoded.extend_from_slice(&scaled.to_le_bytes()[..3]);
                }
                file.write_all(&encoded)?;
                at += frames as u64;
            }
            file.sync_all()?;
            Ok::<_, Error>(())
        })();
        drop(file);
        result?;
        staged.publish_new(&destination)
    }
}

pub struct RecordingWriter {
    file: File,
    destination: PathBuf,
    temp: PathBuf,
    channels: u16,
    frames: u64,
}
impl RecordingWriter {
    /// Accepts already-captured or synthetic 48 kHz interleaved float frames.
    /// It never accesses a system input device.
    pub fn new(destination: &Path, channels: u16) -> Result<Self> {
        if !(1..=2).contains(&channels) { return Err(Error::Invalid("recording needs mono or stereo frames".into())); }
        let name = destination.file_name()
            .ok_or_else(|| Error::Invalid("recording destination needs a file name".into()))?;
        let parent = destination.parent().filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = fs::canonicalize(parent)?;
        reject_project_tree(&parent)?;
        let destination = parent.join(name);
        let temp = destination.with_extension("audiostrike-record.tmp");
        let backup = destination.with_extension("audiostrike-record.bak");
        if temp.exists() || backup.exists() { return Err(Error::Invalid("recover existing recording temp or backup".into())); }
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        write_header(&mut file, channels, 0, 3, 32)?;
        Ok(Self { file, destination, temp, channels, frames: 0 })
    }
    pub fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        if interleaved.len() % self.channels as usize != 0 || interleaved.iter().any(|x| !x.is_finite()) {
            return Err(Error::Invalid("recording frames are misaligned or non-finite".into()));
        }
        let frames = interleaved.len() / self.channels as usize;
        if self.frames.checked_add(frames as u64).is_none_or(|n| n > MAX_FRAMES) {
            return Err(Error::Invalid("recording exceeds one hour".into()));
        }
        for sample in interleaved { self.file.write_all(&sample.to_le_bytes())?; }
        self.frames += frames as u64;
        Ok(())
    }
    pub fn finish(mut self) -> Result<u64> {
        write_header(&mut self.file, self.channels, self.frames, 3, 32)?;
        self.file.sync_all()?;
        let backup = self.destination.with_extension("audiostrike-record.bak");
        replace_recording_with_backup(&self.temp, &self.destination, &backup)?;
        Ok(self.frames)
    }
}

#[cfg(test)]
mod malformed_media_tests {
    use super::*;
    #[test]
    fn direct_media_reader_refuses_overflowing_frame_metadata() {
        let file = std::env::temp_dir().join(format!("audiostrike-overflow-reader-{}.wav", std::process::id()));
        crate::write_wav_float(&file, &crate::WavData {
            sample_rate: RATE, channels: 1, samples: vec![0.0],
        }).unwrap();
        let result = MediaReader::open(&file, u64::MAX, 1);
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert!(file.is_file());
    }
}
