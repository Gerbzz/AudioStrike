//! Tempo and meter maps: the conversion between musical time (ticks) and samples.

use crate::{SampleRate, Samples, TimeError, to_samples};

/// Ticks per quarter note (Pro Tools' resolution).
pub const TICKS_PER_QUARTER: i64 = 960;

/// A tempo change at a tick position. `bpm` is quarter notes per minute.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TempoEvent {
    pub tick: i64,
    pub bpm: f64,
}

/// A meter (time signature) change at a tick position, which must fall on a bar line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MeterEvent {
    pub tick: i64,
    pub numerator: u32,
    pub denominator: u32,
}

impl MeterEvent {
    pub fn ticks_per_beat(&self) -> i64 {
        let d = i64::from(self.denominator.max(1));
        (TICKS_PER_QUARTER * 4 / d).max(1)
    }
    pub fn ticks_per_bar(&self) -> i64 {
        self.ticks_per_beat().saturating_mul(i64::from(self.numerator.max(1)))
    }
}

/// A musical position: 1-based bar and beat plus ticks within the beat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BarBeat {
    pub bar: i64,
    pub beat: i64,
    pub tick: i64,
}

/// Tempo and meter changes for a session. Always has at least one of each, at tick 0.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TempoMap {
    tempos: Vec<TempoEvent>,
    meters: Vec<MeterEvent>,
}

impl Default for TempoMap {
    fn default() -> Self {
        TempoMap { tempos: vec![TempoEvent { tick: 0, bpm: 120.0 }], meters: vec![MeterEvent { tick: 0, numerator: 4, denominator: 4 }] }
    }
}

fn valid_bpm(bpm: f64) -> Result<f64, TimeError> {
    if bpm.is_finite() && (5.0..=1000.0).contains(&bpm) { Ok(bpm) } else { Err(TimeError::Tempo(bpm)) }
}

impl TempoMap {
    pub fn new(bpm: f64, numerator: u32, denominator: u32) -> Result<Self, TimeError> {
        let mut m = TempoMap::default();
        m.set_tempo(0, bpm)?;
        m.set_meter(0, numerator, denominator)?;
        Ok(m)
    }

    pub fn tempos(&self) -> &[TempoEvent] {
        &self.tempos
    }
    pub fn meters(&self) -> &[MeterEvent] {
        &self.meters
    }

    /// Insert or replace the tempo at `tick`.
    pub fn set_tempo(&mut self, tick: i64, bpm: f64) -> Result<(), TimeError> {
        let bpm = valid_bpm(bpm)?;
        let tick = tick.max(0);
        match self.tempos.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => {
                if let Some(e) = self.tempos.get_mut(i) {
                    e.bpm = bpm;
                }
            }
            Err(i) => self.tempos.insert(i, TempoEvent { tick, bpm }),
        }
        Ok(())
    }

    /// Remove a tempo event (the one at tick 0 is kept).
    pub fn remove_tempo(&mut self, tick: i64) -> bool {
        if tick == 0 {
            return false;
        }
        let before = self.tempos.len();
        self.tempos.retain(|e| e.tick != tick);
        before != self.tempos.len()
    }

    /// Insert or replace the meter at `tick` (snapped to the bar line at or before it).
    pub fn set_meter(&mut self, tick: i64, numerator: u32, denominator: u32) -> Result<(), TimeError> {
        if !(1..=64).contains(&numerator) || ![1, 2, 4, 8, 16, 32, 64].contains(&denominator) {
            return Err(TimeError::Meter(numerator, denominator));
        }
        let tick = if tick <= 0 { 0 } else { self.bar_start_tick(self.bar_beat_at_tick(tick).bar) };
        let ev = MeterEvent { tick, numerator, denominator };
        match self.meters.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => {
                if let Some(e) = self.meters.get_mut(i) {
                    *e = ev;
                }
            }
            Err(i) => self.meters.insert(i, ev),
        }
        Ok(())
    }

    pub fn remove_meter(&mut self, tick: i64) -> bool {
        if tick == 0 {
            return false;
        }
        let before = self.meters.len();
        self.meters.retain(|e| e.tick != tick);
        before != self.meters.len()
    }

    /// Tempo in effect at `tick`.
    pub fn tempo_at_tick(&self, tick: i64) -> f64 {
        let mut bpm = self.tempos.first().map_or(120.0, |e| e.bpm);
        for e in &self.tempos {
            if e.tick <= tick {
                bpm = e.bpm;
            } else {
                break;
            }
        }
        bpm
    }

    /// Meter in effect at `tick`.
    pub fn meter_at_tick(&self, tick: i64) -> MeterEvent {
        let mut m = self.meters.first().copied().unwrap_or(MeterEvent { tick: 0, numerator: 4, denominator: 4 });
        for e in &self.meters {
            if e.tick <= tick {
                m = *e;
            } else {
                break;
            }
        }
        m
    }

    fn samples_per_tick(bpm: f64, sr: SampleRate) -> f64 {
        sr.as_f64() * 60.0 / (bpm * TICKS_PER_QUARTER as f64)
    }

    /// Ticks → samples (exact for constant-tempo segments).
    pub fn tick_to_samples(&self, tick: i64, sr: SampleRate) -> Samples {
        to_samples(self.tick_to_samples_f(tick, sr))
    }

    fn tick_to_samples_f(&self, tick: i64, sr: SampleRate) -> f64 {
        let mut acc = 0.0;
        let mut seg_tick = 0i64;
        let mut bpm = self.tempos.first().map_or(120.0, |e| e.bpm);
        for e in self.tempos.iter().skip(1) {
            if e.tick >= tick {
                break;
            }
            acc += (e.tick - seg_tick) as f64 * Self::samples_per_tick(bpm, sr);
            seg_tick = e.tick;
            bpm = e.bpm;
        }
        acc + (tick - seg_tick) as f64 * Self::samples_per_tick(bpm, sr)
    }

    /// Samples → ticks (fractional).
    pub fn samples_to_ticks_f(&self, s: Samples, sr: SampleRate) -> f64 {
        let s = s as f64;
        let mut acc = 0.0;
        let mut seg_tick = 0i64;
        let mut bpm = self.tempos.first().map_or(120.0, |e| e.bpm);
        for e in self.tempos.iter().skip(1) {
            let seg = (e.tick - seg_tick) as f64 * Self::samples_per_tick(bpm, sr);
            if acc + seg > s {
                break;
            }
            acc += seg;
            seg_tick = e.tick;
            bpm = e.bpm;
        }
        seg_tick as f64 + (s - acc) / Self::samples_per_tick(bpm, sr)
    }

    pub fn samples_to_ticks(&self, s: Samples, sr: SampleRate) -> i64 {
        to_samples(self.samples_to_ticks_f(s, sr))
    }

    /// Tick of the first beat of `bar` (1-based; bars before 1 extend the first meter backwards).
    pub fn bar_start_tick(&self, bar: i64) -> i64 {
        let mut cur_bar = 1i64;
        let mut cur_tick = 0i64;
        let first = self.meter_at_tick(0);
        if bar < 1 {
            return (bar - 1).saturating_mul(first.ticks_per_bar());
        }
        for (i, m) in self.meters.iter().enumerate() {
            let next_tick = self.meters.get(i + 1).map(|n| n.tick);
            let tpb = m.ticks_per_bar();
            let bars_in_seg = next_tick.map(|nt| ((nt - cur_tick) + tpb - 1) / tpb);
            match bars_in_seg {
                Some(n) if bar >= cur_bar + n => {
                    cur_bar += n;
                    cur_tick = next_tick.unwrap_or(cur_tick);
                }
                _ => return cur_tick.saturating_add((bar - cur_bar).saturating_mul(tpb)),
            }
        }
        cur_tick
    }

    /// Bar|beat|tick at `tick`.
    pub fn bar_beat_at_tick(&self, tick: i64) -> BarBeat {
        let first = self.meter_at_tick(0);
        if tick < 0 {
            let tpb = first.ticks_per_bar();
            let bars_back = (-tick + tpb - 1) / tpb;
            let bar_tick = -bars_back * tpb;
            let within = tick - bar_tick;
            let beat_t = first.ticks_per_beat();
            return BarBeat { bar: 1 - bars_back, beat: within / beat_t + 1, tick: within % beat_t };
        }
        let mut bar = 1i64;
        let mut seg_tick = 0i64;
        for (i, m) in self.meters.iter().enumerate() {
            let tpb = m.ticks_per_bar();
            if let Some(n) = self.meters.get(i + 1)
                && tick >= n.tick
            {
                bar += ((n.tick - seg_tick) + tpb - 1) / tpb;
                seg_tick = n.tick;
                continue;
            }
            let rel = tick - seg_tick;
            let bars = rel / tpb;
            let within = rel % tpb;
            let beat_t = m.ticks_per_beat();
            return BarBeat { bar: bar + bars, beat: within / beat_t + 1, tick: within % beat_t };
        }
        BarBeat { bar, beat: 1, tick: 0 }
    }

    /// Bar|beat|tick → tick.
    pub fn tick_at_bar_beat(&self, bb: BarBeat) -> i64 {
        let start = self.bar_start_tick(bb.bar);
        let m = self.meter_at_tick(start);
        start.saturating_add((bb.beat - 1).max(0).saturating_mul(m.ticks_per_beat())).saturating_add(bb.tick)
    }

    pub fn bar_beat_at(&self, s: Samples, sr: SampleRate) -> BarBeat {
        self.bar_beat_at_tick(self.samples_to_ticks(s, sr))
    }

    pub fn samples_at_bar_beat(&self, bb: BarBeat, sr: SampleRate) -> Samples {
        self.tick_to_samples(self.tick_at_bar_beat(bb), sr)
    }

    /// Scale every tempo by `factor` (Tempo Operations › Scale).
    pub fn scale_tempos(&mut self, factor: f64) -> Result<(), TimeError> {
        let mut out = self.tempos.clone();
        for e in &mut out {
            e.bpm = valid_bpm(e.bpm * factor)?;
        }
        self.tempos = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_map_is_120_4_4() {
        let m = TempoMap::default();
        let sr = SampleRate::HZ_48000;
        // One bar of 4/4 at 120 bpm = 2 seconds.
        assert_eq!(m.samples_at_bar_beat(BarBeat { bar: 2, beat: 1, tick: 0 }, sr), 96_000);
        assert_eq!(m.bar_beat_at(96_000, sr), BarBeat { bar: 2, beat: 1, tick: 0 });
        assert_eq!(m.bar_beat_at(24_000, sr), BarBeat { bar: 1, beat: 2, tick: 0 });
    }

    #[test]
    fn tempo_change_is_integrated() {
        let mut m = TempoMap::default();
        let sr = SampleRate::HZ_48000;
        m.set_tempo(TICKS_PER_QUARTER * 4, 60.0).unwrap();
        // Bar 1 at 120 = 96000 samples; bar 2 at 60 = 192000 samples.
        assert_eq!(m.tick_to_samples(TICKS_PER_QUARTER * 8, sr), 96_000 + 192_000);
        assert_eq!(m.samples_to_ticks(96_000 + 192_000, sr), TICKS_PER_QUARTER * 8);
    }

    #[test]
    fn meter_change_counts_bars() {
        let mut m = TempoMap::default();
        // 2 bars of 4/4 then 3/4.
        m.set_meter(TICKS_PER_QUARTER * 8, 3, 4).unwrap();
        assert_eq!(m.bar_beat_at_tick(TICKS_PER_QUARTER * 8), BarBeat { bar: 3, beat: 1, tick: 0 });
        assert_eq!(m.bar_beat_at_tick(TICKS_PER_QUARTER * 11), BarBeat { bar: 4, beat: 1, tick: 0 });
        assert_eq!(m.bar_start_tick(4), TICKS_PER_QUARTER * 11);
        let bb = BarBeat { bar: 4, beat: 2, tick: 10 };
        assert_eq!(m.bar_beat_at_tick(m.tick_at_bar_beat(bb)), bb);
    }

    #[test]
    fn eighth_note_meters() {
        let m = TempoMap::new(120.0, 6, 8).unwrap();
        assert_eq!(m.bar_beat_at_tick(TICKS_PER_QUARTER * 3), BarBeat { bar: 2, beat: 1, tick: 0 });
        assert_eq!(m.bar_beat_at_tick(TICKS_PER_QUARTER / 2), BarBeat { bar: 1, beat: 2, tick: 0 });
    }

    #[test]
    fn rejects_bad_values() {
        let mut m = TempoMap::default();
        assert!(m.set_tempo(0, f64::NAN).is_err());
        assert!(m.set_tempo(0, 0.0).is_err());
        assert!(m.set_meter(0, 0, 4).is_err());
        assert!(m.set_meter(0, 4, 3).is_err());
        assert!(!m.remove_tempo(0));
    }

    #[test]
    fn negative_positions() {
        let m = TempoMap::default();
        assert_eq!(m.bar_beat_at_tick(-TICKS_PER_QUARTER * 4), BarBeat { bar: 0, beat: 1, tick: 0 });
        assert_eq!(m.bar_beat_at_tick(-TICKS_PER_QUARTER), BarBeat { bar: 0, beat: 4, tick: 0 });
    }
}
