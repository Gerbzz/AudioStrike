//! AudioSuite-style whole-buffer processing on planar `f32` audio.
//!
//! These functions may allocate (they are not for the audio thread) but never panic: lengths,
//! rates and ratios are sanitized and output sizes are capped at [`MAX_OFFLINE_SAMPLES`] per
//! channel.

use std::f64::consts::PI;

use rustfft::{FftPlanner, num_complex::Complex};

use crate::osc::Osc;
pub use crate::osc::Waveform;
use crate::util::sane_sr;
use crate::{Plugin, db_to_gain, gain_to_db};

/// Largest number of samples per channel any offline function will produce.
pub const MAX_OFFLINE_SAMPLES: usize = 1 << 28;

/// Fade curves. [`FadeShape::gain`] is the fade-in curve; a fade-out uses `gain(1 - t)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum FadeShape {
    Linear,
    /// Constant power (sin/cos), for crossfades between uncorrelated material.
    #[default]
    EqualPower,
    /// Raised cosine.
    SCurve,
    /// Slow start, fast finish.
    Exponential,
    /// Fast start, slow finish.
    Logarithmic,
}

impl FadeShape {
    pub const ALL: [FadeShape; 5] = [FadeShape::Linear, FadeShape::EqualPower, FadeShape::SCurve, FadeShape::Exponential, FadeShape::Logarithmic];

    /// Fade-in gain at position `t` in 0..1 (0 → silent, 1 → unity). NaN is treated as 0.
    pub fn gain(self, t: f32) -> f32 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        const K: f32 = 4.0;
        match self {
            FadeShape::Linear => t,
            FadeShape::EqualPower => (t * std::f32::consts::FRAC_PI_2).sin(),
            FadeShape::SCurve => 0.5 - 0.5 * (t * std::f32::consts::PI).cos(),
            FadeShape::Exponential => ((K * t).exp() - 1.0) / (K.exp() - 1.0),
            FadeShape::Logarithmic => (1.0 + (K.exp() - 1.0) * t).ln() / K,
        }
    }
}

fn max_len(ch: &[Vec<f32>]) -> usize {
    ch.iter().map(Vec::len).max().unwrap_or(0)
}

fn peak(ch: &[Vec<f32>]) -> f32 {
    ch.iter().flatten().filter(|v| v.is_finite()).fold(0.0f32, |m, v| m.max(v.abs()))
}

/// Peak-normalizes to `target_db` dBFS, either all channels together or each on its own.
/// Silent channels are left alone.
pub fn normalize(ch: &mut [Vec<f32>], target_db: f32, per_channel: bool) {
    let target = db_to_gain(if target_db.is_nan() { 0.0 } else { target_db.clamp(-144.0, 24.0) });
    if per_channel {
        for c in ch.iter_mut() {
            let p = peak(std::slice::from_ref(c));
            if p > 0.0 {
                let g = target / p;
                c.iter_mut().for_each(|s| *s *= g);
            }
        }
    } else {
        let p = peak(ch);
        if p > 0.0 {
            let g = target / p;
            ch.iter_mut().flatten().for_each(|s| *s *= g);
        }
    }
}

/// Applies a gain change in dB.
pub fn gain(ch: &mut [Vec<f32>], db: f32) {
    let g = db_to_gain(if db.is_nan() { 0.0 } else { db.min(96.0) });
    ch.iter_mut().flatten().for_each(|s| *s *= g);
}

/// Flips polarity.
pub fn invert(ch: &mut [Vec<f32>]) {
    ch.iter_mut().flatten().for_each(|s| *s = -*s);
}

/// Reverses each channel in time.
pub fn reverse(ch: &mut [Vec<f32>]) {
    ch.iter_mut().for_each(|c| c.reverse());
}

/// Removes each channel's DC offset (subtracts its mean).
pub fn remove_dc(ch: &mut [Vec<f32>]) {
    for c in ch.iter_mut() {
        if c.is_empty() {
            continue;
        }
        let mean = c.iter().filter(|v| v.is_finite()).map(|&v| f64::from(v)).sum::<f64>() / c.len() as f64;
        let m = mean as f32;
        c.iter_mut().for_each(|s| *s -= m);
    }
}

/// Fades in over the first `fade_in` samples and out over the last `fade_out` samples.
pub fn fade(ch: &mut [Vec<f32>], fade_in: usize, fade_out: usize, shape: FadeShape) {
    for c in ch.iter_mut() {
        let len = c.len();
        let fi = fade_in.min(len);
        for (i, s) in c.iter_mut().take(fi).enumerate() {
            *s *= shape.gain(i as f32 / fi as f32);
        }
        let fo = fade_out.min(len);
        for (j, s) in c.iter_mut().skip(len - fo).enumerate() {
            *s *= shape.gain((fo - 1 - j) as f32 / fo as f32);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Resampling (Kaiser-windowed sinc)

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..64 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

const SINC_ZC: f64 = 32.0;
const SINC_RES: usize = 512;

/// Precomputed one-sided windowed-sinc kernel for a given cutoff (fraction of input Nyquist).
struct SincTable {
    table: Vec<f32>,
    half_width: f64,
}

impl SincTable {
    fn new(cutoff: f64) -> Self {
        let cutoff = cutoff.clamp(0.01, 1.0);
        let half_width = SINC_ZC / cutoff;
        let n = (half_width * SINC_RES as f64).ceil() as usize + 2;
        let beta = 9.0;
        let i0b = bessel_i0(beta);
        let table = (0..n)
            .map(|i| {
                let t = i as f64 / SINC_RES as f64;
                let x = t / half_width;
                if x >= 1.0 {
                    return 0.0;
                }
                let arg = PI * cutoff * t;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { arg.sin() / arg };
                let w = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
                (cutoff * sinc * w) as f32
            })
            .collect();
        Self { table, half_width }
    }

    #[inline]
    fn at(&self, t: f64) -> f32 {
        let pos = t.abs() * SINC_RES as f64;
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        match (self.table.get(i), self.table.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * f,
            (Some(a), None) => *a,
            _ => 0.0,
        }
    }
}

/// Resamples one channel to exactly `out_len` samples, where output sample `n` is read at input
/// time `n / ratio`.
fn resample_channel(x: &[f32], ratio: f64, out_len: usize, table: &SincTable) -> Vec<f32> {
    let len = x.len() as i64;
    let hw = table.half_width;
    (0..out_len)
        .map(|n| {
            let t = n as f64 / ratio;
            let lo = (t - hw).ceil().max(0.0) as i64;
            let hi = ((t + hw).floor() as i64).min(len - 1);
            let mut acc = 0.0f32;
            let mut j = lo;
            while j <= hi {
                if let Some(&v) = x.get(j as usize) {
                    acc += v * table.at(t - j as f64);
                }
                j += 1;
            }
            acc
        })
        .collect()
}

fn resample_ratio(ch: &[Vec<f32>], ratio: f64, out_len: usize) -> Vec<Vec<f32>> {
    if !ratio.is_finite() || ratio <= 0.0 {
        return ch.iter().map(|_| vec![0.0; out_len]).collect();
    }
    // Anti-alias below the lower of the two Nyquists, with a little transition room.
    let cutoff = if ratio < 1.0 { ratio * 0.94 } else { 0.97 };
    let table = SincTable::new(cutoff);
    ch.iter().map(|c| resample_channel(c, ratio, out_len, &table)).collect()
}

/// High-quality windowed-sinc sample-rate conversion from `from` Hz to `to` Hz.
pub fn resample(ch: &[Vec<f32>], from: u32, to: u32) -> Vec<Vec<f32>> {
    if from == 0 || to == 0 || from == to {
        return ch.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    ch.iter()
        .map(|c| {
            let out_len = ((c.len() as f64 * ratio).round() as usize).min(MAX_OFFLINE_SAMPLES);
            resample_ratio(std::slice::from_ref(c), ratio, out_len).into_iter().next().unwrap_or_default()
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Time stretch (WSOLA) and pitch shift

/// Pitch-preserving time stretch by WSOLA (waveform-similarity overlap-add).
/// `ratio` = output length / input length, clamped to 0.1 ..= 10. All channels share the same
/// segment choices so the stereo image stays coherent.
pub fn time_stretch(ch: &[Vec<f32>], ratio: f64, sample_rate: f32) -> Vec<Vec<f32>> {
    let len = max_len(ch);
    let ratio = if ratio.is_finite() { ratio.clamp(0.1, 10.0) } else { 1.0 };
    let out_len = ((len as f64 * ratio).round() as usize).min(MAX_OFFLINE_SAMPLES);
    if len == 0 || ch.is_empty() {
        return ch.iter().map(|_| vec![0.0; out_len]).collect();
    }
    if (ratio - 1.0).abs() < 1e-9 {
        return ch.iter().map(|c| c.iter().copied().chain(std::iter::repeat(0.0)).take(out_len).collect()).collect();
    }
    let sr = f64::from(sane_sr(sample_rate));
    let frame = (((sr * 0.04) as usize) & !1).clamp(64, 8192);
    let hop = frame / 2;
    let tol = ((sr * 0.010) as usize).clamp(8, 2048) as i64;
    let window: Vec<f32> = (0..frame).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / frame as f64).cos()) as f32).collect();
    let nch = ch.len() as f32;
    let guide: Vec<f32> = (0..len).map(|i| ch.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / nch).collect();
    let g = |i: i64| -> f32 { if i < 0 { 0.0 } else { guide.get(i as usize).copied().unwrap_or(0.0) } };

    let buf_len = out_len + frame;
    let mut out: Vec<Vec<f32>> = ch.iter().map(|_| vec![0.0; buf_len]).collect();
    let mut wsum = vec![0.0f32; buf_len];
    let mut prev_in: i64 = 0;
    let mut k = 0usize;
    loop {
        let out_pos = k * hop;
        if out_pos >= out_len {
            break;
        }
        let in_pos = if k == 0 {
            0
        } else {
            let nominal = (out_pos as f64 / ratio).round() as i64;
            let natural = prev_in + hop as i64;
            // Similarity of candidate start `p` with the natural continuation (stride 2).
            let score = |p: i64| -> f32 {
                let (mut xy, mut yy) = (0.0f32, 1e-9f32);
                let mut i = 0i64;
                while i < hop as i64 {
                    let a = g(natural + i);
                    let b = g(p + i);
                    xy += a * b;
                    yy += b * b;
                    i += 2;
                }
                xy / yy.sqrt()
            };
            let lo = (nominal - tol).max(0);
            let hi = nominal + tol;
            let mut best = nominal.max(0);
            let mut best_s = f32::NEG_INFINITY;
            let mut p = lo;
            while p <= hi {
                let s = score(p);
                if s > best_s {
                    best_s = s;
                    best = p;
                }
                p += 4;
            }
            let (rlo, rhi) = ((best - 3).max(lo), (best + 3).min(hi));
            for p in rlo..=rhi {
                let s = score(p);
                if s > best_s {
                    best_s = s;
                    best = p;
                }
            }
            best
        };
        for (c, o) in ch.iter().zip(out.iter_mut()) {
            for (i, w) in window.iter().enumerate() {
                let src = in_pos as usize + i;
                let v = c.get(src).copied().unwrap_or(0.0);
                if let Some(d) = o.get_mut(out_pos + i) {
                    *d += v * w;
                }
            }
        }
        for (i, w) in window.iter().enumerate() {
            if let Some(d) = wsum.get_mut(out_pos + i) {
                *d += w;
            }
        }
        prev_in = in_pos;
        k += 1;
    }
    for o in out.iter_mut() {
        for (s, w) in o.iter_mut().zip(&wsum) {
            if *w > 1e-3 {
                *s /= w;
            }
        }
        o.truncate(out_len);
    }
    out
}

/// Length-preserving pitch shift by `semitones` (clamped to ±48): WSOLA stretch followed by
/// windowed-sinc resampling back to the original length.
pub fn pitch_shift(ch: &[Vec<f32>], semitones: f32, sample_rate: f32) -> Vec<Vec<f32>> {
    let st = if semitones.is_finite() { semitones.clamp(-48.0, 48.0) } else { 0.0 };
    if st.abs() < 1e-4 {
        return ch.to_vec();
    }
    let factor = 2f64.powf(f64::from(st) / 12.0);
    let stretched = time_stretch(ch, factor, sample_rate);
    let len = max_len(ch);
    let slen = max_len(&stretched);
    if slen == 0 || len == 0 {
        return ch.iter().map(|c| vec![0.0; c.len()]).collect();
    }
    let ratio = len as f64 / slen as f64;
    let mut out = resample_ratio(&stretched, ratio, len);
    for (o, c) in out.iter_mut().zip(ch) {
        o.truncate(c.len());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Generators and analysis

/// Generates a test signal. Periodic waveforms peak at `level_db`; noise is scaled so its peak
/// stays at or below `level_db`.
pub fn signal_generator(kind: Waveform, freq: f32, level_db: f32, sample_rate: f32, frames: usize, channels: usize) -> Vec<Vec<f32>> {
    let sr = sane_sr(sample_rate);
    let frames = frames.min(MAX_OFFLINE_SAMPLES);
    let channels = channels.min(crate::util::MAX_CHANNELS);
    let f = if freq.is_finite() { freq.clamp(0.0, sr * 0.49) } else { 1000.0 };
    let g = db_to_gain(if level_db.is_nan() { -20.0 } else { level_db.min(0.0) });
    let mut osc = Osc::new(0x1234_5678);
    let mono: Vec<f32> = (0..frames).map(|_| osc.next(kind, f / sr) * g).collect();
    (0..channels).map(|_| mono.clone()).collect()
}

/// Onset detection by half-wave-rectified spectral flux with an adaptive threshold.
/// `sensitivity` 0..1 (higher finds more transients). Returns sample positions, ascending.
pub fn detect_transients(ch: &[Vec<f32>], sample_rate: f32, sensitivity: f32) -> Vec<usize> {
    let len = max_len(ch);
    let sr = sane_sr(sample_rate);
    let sens = if sensitivity.is_finite() { sensitivity.clamp(0.0, 1.0) } else { 0.5 };
    let n = ((sr * 0.023) as usize).clamp(64, 8192).next_power_of_two();
    let hop = n / 4;
    if len < n || ch.is_empty() {
        return Vec::new();
    }
    let nch = ch.len() as f32;
    let mono: Vec<f32> = (0..len).map(|i| ch.iter().map(|c| c.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0)).sum::<f32>() / nch).collect();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let window: Vec<f32> = (0..n).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32).collect();
    let frames = (len - n) / hop + 1;
    let mut prev = vec![0.0f32; n / 2 + 1];
    let mut buf = vec![Complex::new(0.0f32, 0.0); n];
    let mut flux = Vec::with_capacity(frames);
    for m in 0..frames {
        let start = m * hop;
        for (i, b) in buf.iter_mut().enumerate() {
            *b = Complex::new(mono.get(start + i).copied().unwrap_or(0.0) * window.get(i).copied().unwrap_or(0.0), 0.0);
        }
        fft.process(&mut buf);
        let mut f = 0.0;
        for (b, p) in buf.iter().zip(prev.iter_mut()) {
            let mag = (1.0 + 100.0 * b.norm()).ln();
            f += (mag - *p).max(0.0);
            *p = mag;
        }
        flux.push(if m == 0 { 0.0 } else { f });
    }
    let max_flux = flux.iter().copied().fold(0.0f32, f32::max);
    if max_flux <= 1e-6 {
        return Vec::new();
    }
    let w = ((0.1 * sr) as usize / hop).max(2);
    let mult = 1.0 + 2.5 * (1.0 - sens);
    let delta = max_flux * (0.02 + 0.25 * (1.0 - sens));
    let min_gap = ((0.05 * sr) as usize / hop).max(1);
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for m in 1..flux.len() {
        let cur = flux.get(m).copied().unwrap_or(0.0);
        let lo = m.saturating_sub(w);
        let hi = (m + w + 1).min(flux.len());
        let local = flux.get(lo..hi).map(|s| s.iter().sum::<f32>() / s.len().max(1) as f32).unwrap_or(0.0);
        let prev_v = flux.get(m - 1).copied().unwrap_or(0.0);
        let next_v = flux.get(m + 1).copied().unwrap_or(0.0);
        if cur > local * mult + delta && cur >= prev_v && cur > next_v && last.is_none_or(|l| m - l >= min_gap) {
            last = Some(m);
            // Refine: first sample in the analysis frame reaching a quarter of the frame's peak.
            let start = m * hop;
            let seg = mono.get(start..(start + n).min(len)).unwrap_or(&[]);
            let pk = seg.iter().fold(0.0f32, |a, v| a.max(v.abs()));
            let off = seg.iter().position(|v| v.abs() >= 0.25 * pk).unwrap_or(n / 2);
            let pos = start + off;
            if out.last().is_none_or(|&p: &usize| pos > p) {
                out.push(pos);
            }
        }
    }
    out
}

/// Strip Silence: ranges `(start, end)` (end exclusive) of material whose peak across channels
/// exceeds `threshold_db`. Silent gaps shorter than `min_len` samples do not split a region.
/// Each region is padded by `pad_before`/`pad_after` (clamped to the buffer) and overlapping
/// regions are merged.
pub fn non_silent_ranges(ch: &[Vec<f32>], threshold_db: f32, min_len: usize, pad_before: usize, pad_after: usize) -> Vec<(usize, usize)> {
    let len = max_len(ch);
    let thr = db_to_gain(if threshold_db.is_nan() { -48.0 } else { threshold_db });
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_loud = 0usize;
    for i in 0..len {
        let loud = ch.iter().any(|c| c.get(i).is_some_and(|v| v.is_finite() && v.abs() > thr));
        if !loud {
            continue;
        }
        match start {
            Some(s) if i - last_loud > min_len.max(1) => {
                raw.push((s, last_loud + 1));
                start = Some(i);
            }
            None => start = Some(i),
            _ => {}
        }
        last_loud = i;
    }
    if let Some(s) = start {
        raw.push((s, last_loud + 1));
    }
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(raw.len());
    for (s, e) in raw {
        let s = s.saturating_sub(pad_before);
        let e = e.saturating_add(pad_after).min(len);
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Runs a plugin over whole buffers in blocks, compensating its latency so the output lines up
/// with the input. The buffer length is unchanged (any tail past the end is dropped; see
/// [`apply_plugin_with_tail`]).
pub fn apply_plugin(ch: &mut [Vec<f32>], plugin: &mut dyn Plugin, sample_rate: f32) {
    let len = max_len(ch);
    let out = render(ch, plugin, sample_rate, len);
    for (c, o) in ch.iter_mut().zip(out) {
        for (d, s) in c.iter_mut().zip(o) {
            *d = s;
        }
    }
}

/// Like [`apply_plugin`] but returns new buffers extended by the plugin's tail (capped at 60 s).
pub fn apply_plugin_with_tail(ch: &[Vec<f32>], plugin: &mut dyn Plugin, sample_rate: f32) -> Vec<Vec<f32>> {
    let len = max_len(ch);
    plugin.prepare(sane_sr(sample_rate), 1024, ch.len());
    let tail = plugin.tail_samples().min((sane_sr(sample_rate) * 60.0) as usize);
    render(ch, plugin, sample_rate, (len + tail).min(MAX_OFFLINE_SAMPLES))
}

fn render(ch: &[Vec<f32>], plugin: &mut dyn Plugin, sample_rate: f32, out_len: usize) -> Vec<Vec<f32>> {
    const BLOCK: usize = 1024;
    let sr = sane_sr(sample_rate);
    let channels = ch.len();
    plugin.prepare(sr, BLOCK, channels);
    plugin.reset();
    let lat = plugin.latency().min(MAX_OFFLINE_SAMPLES);
    let mut out: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; out_len]).collect();
    let mut scratch: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; BLOCK]).collect();
    let total = out_len + lat;
    let mut done = 0usize;
    while done < total {
        let n = BLOCK.min(total - done);
        for (s, c) in scratch.iter_mut().zip(ch) {
            for (i, v) in s.iter_mut().enumerate() {
                *v = if i < n { c.get(done + i).copied().unwrap_or(0.0) } else { 0.0 };
            }
        }
        plugin.process(&mut scratch, n);
        for (s, o) in scratch.iter().zip(out.iter_mut()) {
            for (i, v) in s.iter().take(n).enumerate() {
                let idx = done + i;
                if idx >= lat
                    && let Some(d) = o.get_mut(idx - lat)
                {
                    *d = if v.is_finite() { *v } else { 0.0 };
                }
            }
        }
        done += n;
    }
    out
}

/// Peak level of the buffers in dBFS (-144 for silence).
pub fn peak_db(ch: &[Vec<f32>]) -> f32 {
    gain_to_db(peak(ch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_shapes_endpoints() {
        for s in FadeShape::ALL {
            assert!(s.gain(0.0).abs() < 1e-6, "{s:?}");
            assert!((s.gain(1.0) - 1.0).abs() < 1e-5, "{s:?}");
            assert!(s.gain(f32::NAN) == 0.0);
        }
    }

    #[test]
    fn fade_hits_zero_at_edges() {
        let mut ch = vec![vec![1.0f32; 100]];
        fade(&mut ch, 10, 10, FadeShape::Linear);
        assert_eq!(ch[0][0], 0.0);
        assert_eq!(ch[0][99], 0.0);
        assert_eq!(ch[0][50], 1.0);
        fade(&mut ch, 1000, 1000, FadeShape::SCurve);
    }
}
