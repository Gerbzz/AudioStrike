//! Audio-only adaptation of FilmCraft's mixer fader law and timeline view math.
//!
//! Source: story­told/filmcraft commit 2ff3f5e266838700c292a35aaf762e3d4dcef980,
//! crates/ui-egui/src/panels/mixer.rs (`TAPER`, `db_to_pos`, `pos_to_db`) and
//! crates/ui-egui/src/panels/timeline.rs (`timeline_extent`, `max_scroll`,
//! `zoom_about`, `waveform_display_gain`). See THIRD_PARTY.md and LICENSE-MIT.

const FADER_MIN_DB: f64 = -96.0;
const FADER_MAX_DB: f64 = 15.0;

// FilmCraft taper, unchanged; the full UI panel depends on its video project engine.
const TAPER: [(f64, f32); 10] = [
    (-96.0, 0.0),
    (-60.0, 0.05),
    (-40.0, 0.14),
    (-30.0, 0.24),
    (-20.0, 0.38),
    (-12.0, 0.52),
    (-6.0, 0.64),
    (0.0, 0.76),
    (6.0, 0.88),
    (15.0, 1.0),
];

pub fn db_to_pos(db: f64) -> f32 {
    if db <= TAPER[0].0 {
        return 0.0;
    }
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if db <= d1 {
            return p0 + (p1 - p0) * ((db - d0) / (d1 - d0)) as f32;
        }
    }
    1.0
}
pub fn pos_to_db(p: f32) -> f64 {
    let p = p.clamp(0.0, 1.0);
    for w in TAPER.windows(2) {
        let ((d0, p0), (d1, p1)) = (w[0], w[1]);
        if p <= p1 {
            let db = d0 + (d1 - d0) * ((p - p0) / (p1 - p0)) as f64;
            return if db <= FADER_MIN_DB + 0.5 {
                FADER_MIN_DB
            } else {
                db
            };
        }
    }
    FADER_MAX_DB
}

#[derive(Clone, Copy, Debug)]
pub struct TimelineView {
    pub target_pps: f64,
    pub target_scroll: f64,
}

pub fn timeline_extent(sequence_seconds: f64) -> f64 {
    // AudioStrike uses a 30 s tail for songs rather than FilmCraft's 600 s video tail.
    sequence_seconds.max(0.0) + 30.0
}
pub fn max_scroll(sequence_seconds: f64, width: f32, pps: f64) -> f64 {
    if pps <= 0.0 || !pps.is_finite() {
        return 0.0;
    }
    (timeline_extent(sequence_seconds) - width as f64 / pps).max(0.0)
}
pub fn zoom_about(v: &mut TimelineView, factor: f64, anchor_secs: f64, width: f32) {
    let old = v.target_pps;
    let new = (old * factor).clamp(0.05, 24_000.0);
    let x = ((anchor_secs - v.target_scroll) * old) as f32;
    let x = if (0.0..=width).contains(&x) {
        x
    } else {
        width / 2.0
    };
    v.target_pps = new;
    v.target_scroll = (anchor_secs - x as f64 / new).max(0.0);
}

pub fn waveform_display_gain(source_peak: f32, clip_gain_db: f64) -> f32 {
    const MAX_BOOST: f32 = 3.981_072;
    let boost = (1.0 / source_peak.max(1e-9)).min(MAX_BOOST);
    10.0_f32.powf(clip_gain_db as f32 / 20.0) * boost
}
