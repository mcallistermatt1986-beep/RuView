//! Feature extraction (ADR-151 Stage 3).
//!
//! Turns an anchor capture — a per-frame scalar series derived from the
//! baseline-subtracted CSI (mean amplitude or dominant-subcarrier phase) — into
//! a compact [`Features`] vector the small specialists consume. No giant model:
//! the useful signal (variance, motion, periodicity, dominant rhythm) is cheap
//! to compute and is exactly what breathing/heartbeat/posture/presence need.
//!
//! Heartbeat and breathing are tiny *repeating* disturbances in the RF field, so
//! periodicity is estimated by autocorrelation over the relevant band — the same
//! technique that fixed the firmware HR estimator (#987).

use serde::{Deserialize, Serialize};

use crate::anchor::AnchorLabel;

/// Compact per-capture (or per-window) feature vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Features {
    /// Mean of the scalar series (presence / static load).
    pub mean: f32,
    /// Variance of the series (motion / occupancy energy).
    pub variance: f32,
    /// Mean absolute first difference (instantaneous motion proxy).
    pub motion: f32,
    /// Dominant periodicity score in the breathing band [0, 1].
    pub breathing_score: f32,
    /// Dominant breathing frequency (Hz), 0 if none.
    pub breathing_hz: f32,
    /// Dominant periodicity score in the heart-rate band [0, 1].
    pub heart_score: f32,
    /// Dominant heart-rate frequency (Hz), 0 if none.
    pub heart_hz: f32,
}

impl Features {
    /// A fixed-length numeric embedding for nearest-prototype classifiers.
    pub fn embedding(&self) -> [f32; 5] {
        [self.mean, self.variance, self.motion, self.breathing_hz, self.heart_hz]
    }

    /// Squared Euclidean distance between two embeddings.
    pub fn distance2(&self, other: &Features) -> f32 {
        self.embedding()
            .iter()
            .zip(other.embedding().iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum()
    }

    /// Extract features from a per-frame scalar series sampled at `fs` Hz.
    pub fn from_series(series: &[f32], fs: f32) -> Features {
        let n = series.len();
        if n == 0 {
            return Features {
                mean: 0.0,
                variance: 0.0,
                motion: 0.0,
                breathing_score: 0.0,
                breathing_hz: 0.0,
                heart_score: 0.0,
                heart_hz: 0.0,
            };
        }
        let mean = series.iter().copied().sum::<f32>() / n as f32;
        let variance =
            series.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n as f32;
        let motion = if n > 1 {
            series.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (n - 1) as f32
        } else {
            0.0
        };

        // De-mean before periodicity search.
        let centered: Vec<f32> = series.iter().map(|v| v - mean).collect();
        let (breathing_hz, breathing_score) = autocorr_dominant(&centered, fs, 0.1, 0.6);
        let (heart_hz, heart_score) = autocorr_dominant(&centered, fs, 0.8, 3.0);

        Features {
            mean,
            variance,
            motion,
            breathing_score,
            breathing_hz,
            heart_score,
            heart_hz,
        }
    }
}

/// A labelled feature record from an enrollment anchor (ADR-151 Stage 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnchorFeature {
    /// Room scope.
    pub room_id: String,
    /// Which anchor this came from.
    pub label: AnchorLabel,
    /// The extracted features.
    pub features: Features,
}

impl AnchorFeature {
    /// Build from a per-frame scalar series.
    pub fn from_series(
        room_id: impl Into<String>,
        label: AnchorLabel,
        series: &[f32],
        fs: f32,
    ) -> AnchorFeature {
        AnchorFeature {
            room_id: room_id.into(),
            label,
            features: Features::from_series(series, fs),
        }
    }
}

/// Dominant frequency in `[lo_hz, hi_hz]` via autocorrelation, with a normalized
/// peak score in `[0, 1]`. Returns `(0, 0)` if no confident peak.
pub fn autocorr_dominant(sig: &[f32], fs: f32, lo_hz: f32, hi_hz: f32) -> (f32, f32) {
    let n = sig.len();
    if n < 16 || fs <= 0.0 || hi_hz <= lo_hz {
        return (0.0, 0.0);
    }
    let lag_min = ((fs / hi_hz).floor() as usize).max(1);
    let lag_max = ((fs / lo_hz).ceil() as usize).min(n - 1);
    if lag_max <= lag_min + 1 {
        return (0.0, 0.0);
    }

    let r0: f32 = sig.iter().map(|v| v * v).sum();
    if r0 <= 1e-6 {
        return (0.0, 0.0);
    }

    let mut best = 0.0f32;
    let mut best_lag = 0usize;
    for lag in lag_min..=lag_max {
        let mut acc = 0.0f32;
        for i in 0..(n - lag) {
            acc += sig[i] * sig[i + lag];
        }
        if acc > best {
            best = acc;
            best_lag = lag;
        }
    }
    if best_lag == 0 {
        return (0.0, 0.0);
    }
    let score = (best / r0).clamp(0.0, 1.0);
    (fs / best_lag as f32, score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq_hz: f32, fs: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq_hz * i as f32 / fs).sin())
            .collect()
    }

    #[test]
    fn autocorr_finds_breathing_freq() {
        // 0.25 Hz (15 BPM) breathing, sampled at 15 Hz for 20 s.
        let fs = 15.0;
        let s = sine(0.25, fs, (fs * 20.0) as usize);
        let (hz, score) = autocorr_dominant(&s, fs, 0.1, 0.6);
        assert!((hz - 0.25).abs() < 0.05, "got {hz}");
        assert!(score > 0.5, "score {score}");
    }

    #[test]
    fn autocorr_finds_heart_freq() {
        // 1.45 Hz (~87 BPM), sampled at 15 Hz.
        let fs = 15.0;
        let s = sine(1.45, fs, (fs * 20.0) as usize);
        let (hz, _) = autocorr_dominant(&s, fs, 0.8, 3.0);
        assert!((hz * 60.0 - 87.0).abs() < 12.0, "got {} bpm", hz * 60.0);
    }

    #[test]
    fn features_capture_breathing() {
        let fs = 15.0;
        let s = sine(0.3, fs, 300);
        let f = Features::from_series(&s, fs);
        assert!(f.breathing_score > 0.4);
        assert!((f.breathing_hz - 0.3).abs() < 0.06);
    }

    #[test]
    fn motion_distinguishes_still_from_noisy() {
        let still = vec![1.0f32; 200];
        let noisy: Vec<f32> = (0..200).map(|i| if i % 2 == 0 { 0.0 } else { 5.0 }).collect();
        assert!(Features::from_series(&still, 15.0).motion < Features::from_series(&noisy, 15.0).motion);
    }

    #[test]
    fn empty_series_is_safe() {
        let f = Features::from_series(&[], 15.0);
        assert_eq!(f.mean, 0.0);
        assert_eq!(f.breathing_hz, 0.0);
    }
}
