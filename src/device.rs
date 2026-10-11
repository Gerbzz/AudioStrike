//! CPAL output adaptation. Projects and exports remain 48 kHz stereo.
//! The device callback only reads a bounded ring and converts samples/channels.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::filmcraft_resample::StreamResampler;
use crate::stream::{BLOCK_FRAMES, StreamingMixer};

const RATE: u32 = 48_000;
const RING_FRAMES: usize = BLOCK_FRAMES * 4;
const MIN_OUTPUT_RATE: u32 = 8_000;
const MAX_OUTPUT_RATE: u32 = 192_000;

struct FrameRing {
    samples: Box<[AtomicU32]>,
    read: AtomicUsize,
    write: AtomicUsize,
}
impl FrameRing {
    fn new() -> Self {
        Self {
            samples: (0..RING_FRAMES * 2).map(|_| AtomicU32::new(0)).collect(),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }
    }
    fn free_frames(&self) -> usize {
        RING_FRAMES - (self.write.load(Ordering::Relaxed) - self.read.load(Ordering::Acquire))
    }
    fn push(&self, samples: &[f32]) {
        let frames = samples.len() / 2;
        debug_assert_eq!(samples.len() % 2, 0);
        debug_assert!(frames <= self.free_frames());
        let at = self.write.load(Ordering::Relaxed);
        for i in 0..frames {
            let slot = (at + i) % RING_FRAMES * 2;
            self.samples[slot].store(samples[i * 2].to_bits(), Ordering::Relaxed);
            self.samples[slot + 1].store(samples[i * 2 + 1].to_bits(), Ordering::Relaxed);
        }
        self.write.store(at + frames, Ordering::Release);
    }
    fn pop(&self) -> Option<[f32; 2]> {
        let at = self.read.load(Ordering::Relaxed);
        if at >= self.write.load(Ordering::Acquire) { return None; }
        let slot = at % RING_FRAMES * 2;
        let pair = [
            f32::from_bits(self.samples[slot].load(Ordering::Relaxed)),
            f32::from_bits(self.samples[slot + 1].load(Ordering::Relaxed)),
        ];
        self.read.store(at + 1, Ordering::Release);
        Some(pair)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputFormat { F32, F64, I8, I16, I32, I64, U8, U16, U32, U64 }
impl OutputFormat {
    fn from_cpal(format: SampleFormat) -> Option<Self> {
        Some(match format {
            SampleFormat::F32 => Self::F32,
            SampleFormat::F64 => Self::F64,
            SampleFormat::I8 => Self::I8,
            SampleFormat::I16 => Self::I16,
            SampleFormat::I32 => Self::I32,
            SampleFormat::I64 => Self::I64,
            SampleFormat::U8 => Self::U8,
            SampleFormat::U16 => Self::U16,
            SampleFormat::U32 => Self::U32,
            SampleFormat::U64 => Self::U64,
            _ => return None,
        })
    }
    fn rank(self) -> u8 {
        match self {
            Self::F32 => 0, Self::F64 => 1,
            Self::I32 | Self::I64 => 2, Self::I16 => 3,
            Self::U32 | Self::U64 => 4, Self::U16 => 5,
            Self::I8 => 6, Self::U8 => 7,
        }
    }
}

#[derive(Clone, Copy)]
struct OutputCapability {
    min_rate: u32,
    max_rate: u32,
    channels: u16,
    format: OutputFormat,
}
fn choose_output(capabilities: &[Option<OutputCapability>]) -> Option<(usize, u32)> {
    capabilities.iter().enumerate().filter_map(|(index, item)| {
        let cap = item.as_ref()?;
        if !(1..=8).contains(&cap.channels) { return None; }
        let min = cap.min_rate.max(MIN_OUTPUT_RATE);
        let max = cap.max_rate.min(MAX_OUTPUT_RATE);
        if min > max { return None; }
        let rate = RATE.clamp(min, max);
        let channel_rank = if cap.channels == 2 { 0 } else if cap.channels == 1 { 1 } else { 2 };
        Some(((channel_rank, cap.format.rank(), rate.abs_diff(RATE), index), index, rate))
    }).min_by_key(|(score, _, _)| *score).map(|(_, index, rate)| (index, rate))
}

struct ConfiguredDevice {
    device: cpal::Device,
    config: StreamConfig,
    format: OutputFormat,
    name: String,
}
fn configured_output() -> Result<ConfiguredDevice, String> {
    let device = cpal::default_host().default_output_device()
        .ok_or("No default output device. Connect a speaker or headset, then press Play to retry.")?;
    let name = device.name().unwrap_or_else(|_| "default output".into());
    let ranges: Vec<_> = device.supported_output_configs()
        .map_err(|e| format!("Cannot query {name} output modes: {e}. Reconnect the device and retry."))?
        .collect();
    let capabilities: Vec<_> = ranges.iter().map(|range| {
        OutputFormat::from_cpal(range.sample_format()).map(|format| OutputCapability {
            min_rate: range.min_sample_rate().0,
            max_rate: range.max_sample_rate().0,
            channels: range.channels(),
            format,
        })
    }).collect();
    let (index, rate) = choose_output(&capabilities).ok_or_else(|| format!(
        "{name} has no convertible 8–192 kHz output mode with 1–8 channels. Choose another device and retry."
    ))?;
    let format = capabilities[index].unwrap().format;
    let config = ranges.into_iter().nth(index).unwrap().with_sample_rate(SampleRate(rate)).config();
    Ok(ConfiguredDevice { device, config, format, name })
}

#[derive(Default)]
struct PlaybackHealth {
    failed: AtomicBool,
    reason: Mutex<Option<String>>,
}
impl PlaybackHealth {
    fn fail(&self, reason: String) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            if let Ok(mut stored) = self.reason.lock() { *stored = Some(reason); }
        }
    }
    fn failed(&self) -> bool { self.failed.load(Ordering::Acquire) }
    fn reason(&self) -> Option<String> { self.reason.lock().ok().and_then(|r| r.clone()) }
}
fn safe_sample(sample: f32) -> f32 {
    // dasp_sample's integer conversion contract excludes positive 1.0.
    if sample.is_finite() { sample.clamp(-1.0, f32::from_bits(1.0_f32.to_bits() - 1)) } else { 0.0 }
}
fn fill_stream_output<T: cpal::FromSample<f32> + Copy>(
    output: &mut [T], channels: usize, ring: &FrameRing, position: &AtomicUsize,
    health: &PlaybackHealth, underruns: &AtomicUsize,
) {
    if channels == 0 { output.fill(T::from_sample_(0.0)); return; }
    for frame in output.chunks_mut(channels) {
        frame.fill(T::from_sample_(0.0));
        if frame.len() != channels || health.failed() { continue; }
        if let Some([left, right]) = ring.pop() {
            let left = safe_sample(left);
            let right = safe_sample(right);
            if channels == 1 { frame[0] = T::from_sample_((left + right) * 0.5); }
            else { frame[0] = T::from_sample_(left); frame[1] = T::from_sample_(right); }
            position.fetch_add(1, Ordering::Relaxed);
        } else { underruns.fetch_add(1, Ordering::Relaxed); }
    }
}

// Existing device-free buffered callback fixture, retained for callers/tests.
pub fn fill_output_f32(output: &mut [f32], samples: &[f32], channels: usize, position: &AtomicUsize) {
    if channels == 0 { output.fill(0.0); return; }
    for frame in output.chunks_mut(channels) {
        let index = position.fetch_add(1, Ordering::Relaxed);
        let pair = samples.get(index.saturating_mul(2)..index.saturating_mul(2).saturating_add(2));
        frame.fill(0.0);
        if frame.len() != channels { continue; }
        if let Some(pair) = pair {
            if channels == 1 { frame[0] = (pair[0] + pair[1]) * 0.5; }
            else { frame[0] = pair[0]; frame[1] = pair[1]; }
        }
    }
}

enum PlaybackSource { Buffer(Vec<f32>), Mixer(StreamingMixer) }
impl PlaybackSource {
    fn total_frames(&self) -> usize {
        match self { Self::Buffer(samples) => samples.len() / 2, Self::Mixer(mixer) => mixer.total_frames() as usize }
    }
    fn read_into(&mut self, at: i64, output: &mut [f32]) -> Result<(), String> {
        output.fill(0.0);
        let frames = output.len() / 2;
        let prefix = if at < 0 { at.unsigned_abs().min(frames as u64) as usize } else { 0 };
        if prefix == frames { return Ok(()); }
        let start = at.max(0) as usize;
        match self {
            Self::Buffer(samples) => {
                let available = (samples.len() / 2).saturating_sub(start).min(frames - prefix);
                if available > 0 {
                    output[prefix * 2..(prefix + available) * 2]
                        .copy_from_slice(&samples[start * 2..(start + available) * 2]);
                }
            }
            Self::Mixer(mixer) => {
                let mut copied = 0;
                while copied < frames - prefix {
                    let count = BLOCK_FRAMES.min(frames - prefix - copied);
                    let begin = (prefix + copied) * 2;
                    let actual = mixer.read_frames((start + copied) as u64,
                        &mut output[begin..begin + count * 2]).map_err(|e| e.to_string())?;
                    if actual < count { break; }
                    copied += actual;
                }
            }
        }
        Ok(())
    }
}

struct OutputProducer {
    source: PlaybackSource,
    resampler: Option<StreamResampler>,
    next_output: usize,
    total_output: usize,
    block: Vec<f32>,
}
impl OutputProducer {
    fn new(source: PlaybackSource, rate: u32, start_frame: usize) -> Result<Self, String> {
        let total = source.total_frames();
        if total == 0 || start_frame >= total { return Err("No audio remains after the playhead.".into()); }
        let convert = |frames: usize| -> Result<usize, String> {
            usize::try_from((frames as u128 * rate as u128).div_ceil(RATE as u128))
                .map_err(|_| "Playback duration exceeds device frame range.".into())
        };
        let next_output = convert(start_frame)?;
        let total_output = convert(total)?;
        if next_output >= total_output { return Err("No device frame remains after the playhead.".into()); }
        Ok(Self {
            source, resampler: (rate != RATE).then(|| StreamResampler::new(RATE, rate)),
            next_output, total_output, block: vec![0.0; BLOCK_FRAMES * 2],
        })
    }
    fn remaining(&self) -> usize { self.total_output - self.next_output }
    fn pump(&mut self, ring: &FrameRing) -> Result<usize, String> {
        let count = self.remaining().min(BLOCK_FRAMES);
        if count == 0 { return Ok(0); }
        if let Some(resampler) = &mut self.resampler {
            let source = &mut self.source;
            let mut read_error = None;
            let planar = resampler.process(self.next_output as i64, count, |at, frames| {
                let mut stereo = vec![0.0; frames * 2];
                if read_error.is_none() {
                    if let Err(error) = source.read_into(at, &mut stereo) { read_error = Some(error); }
                }
                let mut left = Vec::with_capacity(frames);
                let mut right = Vec::with_capacity(frames);
                for pair in stereo.chunks_exact(2) { left.push(pair[0]); right.push(pair[1]); }
                vec![left, right]
            });
            if let Some(error) = read_error { return Err(error); }
            for i in 0..count {
                self.block[i * 2] = planar[0][i];
                self.block[i * 2 + 1] = planar[1][i];
            }
        } else {
            self.source.read_into(self.next_output as i64, &mut self.block[..count * 2])?;
        }
        ring.push(&self.block[..count * 2]);
        self.next_output += count;
        Ok(count)
    }
}

fn build_stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device, config: &StreamConfig, ring: Arc<FrameRing>,
    position: Arc<AtomicUsize>, health: Arc<PlaybackHealth>, underruns: Arc<AtomicUsize>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let callback_health = health.clone();
    device.build_output_stream::<T, _, _>(config,
        move |output: &mut [T], _| {
            fill_stream_output(output, channels, &ring, &position, &callback_health, &underruns);
        },
        move |error| {
            health.fail(format!("Output device stopped: {error}. Reconnect it and press Play to retry."));
        }, None).map_err(|e| format!("Could not open output stream: {e}. Choose another device and retry."))
}

pub struct AudioPlayer {
    _stream: cpal::Stream,
    position: Arc<AtomicUsize>,
    health: Arc<PlaybackHealth>,
    underruns: Arc<AtomicUsize>,
    frames: usize,
    output_frames: usize,
    output_rate: u32,
    pub device_name: String,
    stop_worker: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl AudioPlayer {
    pub fn start(samples: Vec<f32>, start_frame: usize) -> Result<Self, String> {
        if samples.is_empty() || samples.len() % 2 != 0 { return Err("Mix has no stereo audio to play.".into()); }
        Self::start_source(PlaybackSource::Buffer(samples), start_frame)
    }
    pub fn start_streaming(mixer: StreamingMixer, start_frame: usize) -> Result<Self, String> {
        Self::start_source(PlaybackSource::Mixer(mixer), start_frame)
    }
    fn start_source(source: PlaybackSource, start_frame: usize) -> Result<Self, String> {
        let configured = configured_output()?;
        let rate = configured.config.sample_rate.0;
        let mut producer = OutputProducer::new(source, rate, start_frame)?;
        let frames = producer.source.total_frames();
        let output_frames = producer.total_output;
        let start_output = producer.next_output;
        let ring = Arc::new(FrameRing::new());
        producer.pump(&ring).map_err(|e| format!("Cannot read project media for playback: {e}"))?;
        let position = Arc::new(AtomicUsize::new(start_output));
        let health = Arc::new(PlaybackHealth::default());
        let underruns = Arc::new(AtomicUsize::new(0));
        macro_rules! build { ($type:ty) => {
            build_stream::<$type>(&configured.device, &configured.config, ring.clone(),
                position.clone(), health.clone(), underruns.clone())
        }; }
        let stream = match configured.format {
            OutputFormat::F32 => build!(f32), OutputFormat::F64 => build!(f64),
            OutputFormat::I8 => build!(i8), OutputFormat::I16 => build!(i16),
            OutputFormat::I32 => build!(i32), OutputFormat::I64 => build!(i64),
            OutputFormat::U8 => build!(u8), OutputFormat::U16 => build!(u16),
            OutputFormat::U32 => build!(u32), OutputFormat::U64 => build!(u64),
        }?;
        let stop_worker = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_worker.clone();
        let worker_health = health.clone();
        let worker_ring = ring.clone();
        let worker = std::thread::spawn(move || {
            while producer.remaining() > 0 && !worker_stop.load(Ordering::Acquire) && !worker_health.failed() {
                if worker_ring.free_frames() < producer.remaining().min(BLOCK_FRAMES) {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                if let Err(error) = producer.pump(&worker_ring) {
                    worker_health.fail(format!("Project media became unavailable: {error}. Playback stopped; the project was not changed."));
                    break;
                }
            }
        });
        if let Err(error) = stream.play() {
            stop_worker.store(true, Ordering::Release);
            let _ = worker.join();
            return Err(format!("Could not start output on {}: {error}. Reconnect or choose another device, then press Play.", configured.name));
        }
        Ok(Self { _stream: stream, position, health, underruns, frames, output_frames,
            output_rate: rate, device_name: configured.name, stop_worker, worker: Some(worker) })
    }
    pub fn frame(&self) -> usize {
        ((self.position.load(Ordering::Relaxed) as u128 * RATE as u128 / self.output_rate as u128)
            .min(self.frames as u128)) as usize
    }
    pub fn finished(&self) -> bool { self.position.load(Ordering::Relaxed) >= self.output_frames }
    pub fn failed(&self) -> bool { self.health.failed() }
    pub fn failure_reason(&self) -> Option<String> { self.health.reason() }
    pub fn underrun_frames(&self) -> usize { self.underruns.load(Ordering::Relaxed) }
}
impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop_worker.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

pub fn probe_output() -> Result<String, String> {
    let selected = configured_output()?;
    Ok(format!("{}: {} Hz, {} channel(s), {:?}; 48 kHz stereo playback conversion available",
        selected.name, selected.config.sample_rate.0, selected.config.channels, selected.format))
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    #[test]
    fn capability_selection_accepts_44100_and_48000_and_prefers_stereo_quality() {
        let chosen = choose_output(&[
            Some(OutputCapability { min_rate: 44_100, max_rate: 44_100, channels: 1, format: OutputFormat::U16 }),
            Some(OutputCapability { min_rate: 44_100, max_rate: 44_100, channels: 2, format: OutputFormat::F32 }),
            Some(OutputCapability { min_rate: 48_000, max_rate: 48_000, channels: 2, format: OutputFormat::I16 }),
        ]);
        assert_eq!(chosen, Some((1, 44_100)));
        assert_eq!(choose_output(&[Some(OutputCapability { min_rate: 48_000, max_rate: 96_000,
            channels: 2, format: OutputFormat::F32 })]), Some((0, 48_000)));
        assert_eq!(choose_output(&[Some(OutputCapability { min_rate: 1, max_rate: 4_000,
            channels: 2, format: OutputFormat::F32 })]), None);
    }

    #[test]
    fn callback_converts_float_signed_unsigned_and_channel_layout_without_changing_source() {
        let ring = FrameRing::new();
        let position = AtomicUsize::new(0);
        let underruns = AtomicUsize::new(0);
        ring.push(&[-1.0, 1.0, 0.25, -0.25]);
        let mut signed = [0_i16; 4];
        fill_stream_output(&mut signed, 2, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(signed, [i16::MIN, i16::MAX, 8192, -8192]);
        ring.push(&[0.5, -0.5]);
        let mut unsigned = [0_u16; 1];
        fill_stream_output(&mut unsigned, 1, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(unsigned, [32768]);
        ring.push(&[0.25, -0.5]);
        let mut multichannel = [9.0_f32; 4];
        fill_stream_output(&mut multichannel, 4, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(multichannel, [0.25, -0.5, 0.0, 0.0]);
        ring.push(&[0.5, -0.5]);
        let mut wide_signed = [0_i32; 2];
        fill_stream_output(&mut wide_signed, 2, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(wide_signed, [1_073_741_824, -1_073_741_824]);
        ring.push(&[0.0, 0.0]);
        let mut wide_unsigned = [0_u32; 2];
        fill_stream_output(&mut wide_unsigned, 2, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(wide_unsigned, [2_147_483_648; 2]);
        ring.push(&[0.25, -0.25]);
        let mut double = [0.0_f64; 2];
        fill_stream_output(&mut double, 2, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert_eq!(double, [0.25, -0.25]);
        assert_eq!(position.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn every_supported_sample_type_converts_silence_to_its_center() {
        let ring = FrameRing::new();
        ring.push(&[0.0, 0.0]);
        ring.push(&[0.0, 0.0]);
        ring.push(&[0.0, 0.0]);
        ring.push(&[0.0, 0.0]);
        let position = AtomicUsize::new(0);
        let underruns = AtomicUsize::new(0);
        let health = PlaybackHealth::default();
        let mut i8_out = [1_i8; 2];
        let mut u8_out = [1_u8; 2];
        let mut i64_out = [1_i64; 2];
        let mut u64_out = [1_u64; 2];
        fill_stream_output(&mut i8_out, 2, &ring, &position, &health, &underruns);
        fill_stream_output(&mut u8_out, 2, &ring, &position, &health, &underruns);
        fill_stream_output(&mut i64_out, 2, &ring, &position, &health, &underruns);
        fill_stream_output(&mut u64_out, 2, &ring, &position, &health, &underruns);
        assert_eq!(i8_out, [0; 2]);
        assert_eq!(u8_out, [128; 2]);
        assert_eq!(i64_out, [0; 2]);
        assert_eq!(u64_out, [1_u64 << 63; 2]);
        assert_eq!(position.load(Ordering::Relaxed), 4);
        assert_eq!(underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn invalid_or_overrange_samples_stay_finite_and_bounded() {
        assert_eq!(safe_sample(f32::NAN), 0.0);
        assert_eq!(safe_sample(f32::INFINITY), 0.0);
        assert_eq!(safe_sample(f32::NEG_INFINITY), 0.0);
        assert_eq!(safe_sample(-2.0), -1.0);
        assert!(safe_sample(2.0) < 1.0);
    }

    #[test]
    fn direct_48000_output_keeps_the_project_samples() {
        let source = vec![0.1_f32, -0.1, 0.2, -0.2];
        let mut producer = OutputProducer::new(PlaybackSource::Buffer(source.clone()), 48_000, 0).unwrap();
        assert_eq!(producer.total_output, 2);
        assert!(producer.resampler.is_none());
        let ring = FrameRing::new();
        producer.pump(&ring).unwrap();
        assert_eq!(ring.pop(), Some([0.1, -0.1]));
        assert_eq!(ring.pop(), Some([0.2, -0.2]));
        assert_eq!(ring.pop(), None);
        assert_eq!(source, vec![0.1, -0.1, 0.2, -0.2]);
    }

    #[test]
    fn producer_resamples_48k_project_to_44100_device_and_keeps_project_length() {
        let samples: Vec<f32> = (0..480).flat_map(|i| [i as f32 / 480.0, -(i as f32 / 480.0)]).collect();
        let mut producer = OutputProducer::new(PlaybackSource::Buffer(samples), 44_100, 0).unwrap();
        assert_eq!(producer.total_output, 441);
        let ring = FrameRing::new();
        assert_eq!(producer.pump(&ring).unwrap(), 441);
        let position = AtomicUsize::new(0);
        let underruns = AtomicUsize::new(0);
        let mut output = vec![0.0_f32; 441 * 2];
        fill_stream_output(&mut output, 2, &ring, &position, &PlaybackHealth::default(), &underruns);
        assert!(output.iter().all(|s| s.is_finite()));
        assert!(output[400] > 0.0 && output[401] < 0.0);
        assert_eq!(position.load(Ordering::Relaxed), 441);
        assert_eq!(position.load(Ordering::Relaxed) as u128 * RATE as u128 / 44_100, 480);
    }

    #[test]
    fn underrun_and_device_loss_write_silence_without_advancing_playhead() {
        let ring = FrameRing::new();
        let position = AtomicUsize::new(7);
        let underruns = AtomicUsize::new(0);
        let health = PlaybackHealth::default();
        let mut output = [9.0_f32; 4];
        fill_stream_output(&mut output, 2, &ring, &position, &health, &underruns);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(position.load(Ordering::Relaxed), 7);
        assert_eq!(underruns.load(Ordering::Relaxed), 2);
        ring.push(&[0.2, -0.4]);
        health.fail("device lost; reconnect and retry".into());
        fill_stream_output(&mut output, 2, &ring, &position, &health, &underruns);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(position.load(Ordering::Relaxed), 7);
        assert!(health.reason().unwrap().contains("reconnect"));
        assert_eq!(ring.free_frames(), RING_FRAMES - 1);
        // A new Play attempt gets fresh health and a fresh ring at the saved playhead.
        let retry_health = PlaybackHealth::default();
        let retry_ring = FrameRing::new();
        retry_ring.push(&[0.2, -0.4]);
        fill_stream_output(&mut output[..2], 2, &retry_ring, &position, &retry_health, &underruns);
        assert_eq!(output[..2], [0.2, -0.4]);
        assert_eq!(position.load(Ordering::Relaxed), 8);
    }
}
