//! Windows-first local output through CPAL. The callback only reads samples and atomics.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::stream::{BLOCK_FRAMES, StreamingMixer};

const RATE: u32 = 48_000;
const RING_FRAMES: usize = BLOCK_FRAMES * 4;

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

fn fill_stream_output(output: &mut [f32], channels: usize, ring: &FrameRing, position: &AtomicUsize) {
    if channels == 0 { output.fill(0.0); return; }
    for frame in output.chunks_mut(channels) {
        if let Some(pair) = ring.pop() {
            if channels == 1 { frame[0] = (pair[0] + pair[1]) * 0.5; }
            else {
                frame[0] = pair[0]; frame[1] = pair[1];
                frame[2..].fill(0.0);
            }
            position.fetch_add(1, Ordering::Relaxed);
        } else { frame.fill(0.0); }
    }
}

pub fn fill_output_f32(
    output: &mut [f32],
    samples: &[f32],
    channels: usize,
    position: &AtomicUsize,
) {
    if channels == 0 {
        output.fill(0.0);
        return;
    }
    for frame in output.chunks_mut(channels) {
        let index = position.fetch_add(1, Ordering::Relaxed);
        let pair = samples.get(index.saturating_mul(2)..index.saturating_mul(2).saturating_add(2));
        match pair {
            Some(pair) => {
                if channels == 1 {
                    frame[0] = (pair[0] + pair[1]) * 0.5;
                } else {
                    frame[0] = pair[0];
                    frame[1] = pair[1];
                    for extra in &mut frame[2..] {
                        *extra = 0.0;
                    }
                }
            }
            None => frame.fill(0.0),
        }
    }
}

pub struct AudioPlayer {
    _stream: cpal::Stream,
    position: Arc<AtomicUsize>,
    failed: Arc<AtomicBool>,
    frames: usize,
    pub device_name: String,
    stop_worker: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl AudioPlayer {
    pub fn start(samples: Vec<f32>, start_frame: usize) -> Result<Self, String> {
        if samples.is_empty() || samples.len() % 2 != 0 {
            return Err("mix has no stereo audio to play".into());
        }
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default audio output device")?;
        let device_name = device.name().unwrap_or_else(|_| "default output".into());
        let config = device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| {
                c.sample_format() == cpal::SampleFormat::F32
                    && c.min_sample_rate().0 <= RATE
                    && c.max_sample_rate().0 >= RATE
            })
            .min_by_key(|c| {
                if c.channels() == 2 {
                    0
                } else {
                    c.channels() as i32 + 2
                }
            })
            .ok_or("output device has no 48 kHz f32 mode")?
            .with_sample_rate(cpal::SampleRate(RATE))
            .config();
        let channels = config.channels as usize;
        let frames = samples.len() / 2;
        let samples = Arc::new(samples);
        let position = Arc::new(AtomicUsize::new(start_frame.min(frames)));
        let failed = Arc::new(AtomicBool::new(false));
        let play_position = position.clone();
        let play_samples = samples.clone();
        let error_flag = failed.clone();
        let stream = device
            .build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    fill_output_f32(out, &play_samples, channels, &play_position)
                },
                move |_error| {
                    error_flag.store(true, Ordering::Release);
                },
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: stream,
            position,
            failed,
            frames,
            device_name,
            stop_worker: Arc::new(AtomicBool::new(false)),
            worker: None,
        })
    }
    pub fn start_streaming(mut mixer: StreamingMixer, start_frame: usize) -> Result<Self, String> {
        let frames = mixer.total_frames() as usize;
        if frames == 0 || start_frame >= frames {
            return Err("no audio remains after the playhead".into());
        }
        let device = cpal::default_host().default_output_device()
            .ok_or("no default audio output device")?;
        let device_name = device.name().unwrap_or_else(|_| "default output".into());
        let config = device.supported_output_configs().map_err(|e| e.to_string())?
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32
                && c.min_sample_rate().0 <= RATE && c.max_sample_rate().0 >= RATE)
            .min_by_key(|c| if c.channels() == 2 { 0 } else { c.channels() as i32 + 2 })
            .ok_or("output device has no 48 kHz f32 mode")?
            .with_sample_rate(cpal::SampleRate(RATE)).config();
        let channels = config.channels as usize;
        let ring = Arc::new(FrameRing::new());
        let mut block = vec![0.0_f32; BLOCK_FRAMES * 2];
        let first = mixer.read_frames(start_frame as u64, &mut block).map_err(|e| e.to_string())?;
        ring.push(&block[..first * 2]);
        let position = Arc::new(AtomicUsize::new(start_frame));
        let failed = Arc::new(AtomicBool::new(false));
        let callback_ring = ring.clone();
        let callback_position = position.clone();
        let callback_failed = failed.clone();
        let stream = device.build_output_stream(
            &config,
            move |out: &mut [f32], _| fill_stream_output(out, channels, &callback_ring, &callback_position),
            move |_error| { callback_failed.store(true, Ordering::Release); },
            None,
        ).map_err(|e| e.to_string())?;
        let stop_worker = Arc::new(AtomicBool::new(false));
        let worker_stop = stop_worker.clone();
        let worker_failed = failed.clone();
        let worker_ring = ring.clone();
        let worker = std::thread::spawn(move || {
            let mut next = start_frame as u64 + first as u64;
            while next < frames as u64 && !worker_stop.load(Ordering::Acquire) {
                if worker_ring.free_frames() < BLOCK_FRAMES {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                match mixer.read_frames(next, &mut block) {
                    Ok(0) => break,
                    Ok(count) => {
                        worker_ring.push(&block[..count * 2]);
                        next += count as u64;
                    }
                    Err(_) => { worker_failed.store(true, Ordering::Release); break; }
                }
            }
        });
        if let Err(error) = stream.play() {
            stop_worker.store(true, Ordering::Release);
            let _ = worker.join();
            return Err(error.to_string());
        }
        Ok(Self { _stream: stream, position, failed, frames, device_name, stop_worker, worker: Some(worker) })
    }
    pub fn frame(&self) -> usize {
        self.position.load(Ordering::Relaxed).min(self.frames)
    }
    pub fn finished(&self) -> bool {
        self.frame() >= self.frames
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}
impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop_worker.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(test)]
mod ring_tests {
    use super::*;
    #[test]
    fn bounded_ring_drains_frames_and_silences_underflow() {
        let ring = FrameRing::new();
        let position = AtomicUsize::new(7);
        ring.push(&[0.2, -0.4, 0.5, -0.6]);
        let mut output = [9.0_f32; 6];
        fill_stream_output(&mut output, 2, &ring, &position);
        assert_eq!(output, [0.2, -0.4, 0.5, -0.6, 0.0, 0.0]);
        assert_eq!(position.load(Ordering::Relaxed), 9);
        assert_eq!(ring.free_frames(), RING_FRAMES);
    }
}

pub fn probe_output() -> Result<String, String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no default audio output device")?;
    let name = device.name().unwrap_or_else(|_| "default output".into());
    let supported = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .any(|c| {
            c.sample_format() == cpal::SampleFormat::F32
                && c.min_sample_rate().0 <= RATE
                && c.max_sample_rate().0 >= RATE
        });
    if supported {
        Ok(format!("{name}: 48 kHz float output available"))
    } else {
        Err(format!("{name}: no 48 kHz float output mode"))
    }
}
