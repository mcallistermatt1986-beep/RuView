//! Enrollment protocol — per-anchor capture with an adaptive quality gate
//! (ADR-151 Stage 2).
//!
//! Bad anchors poison small calibrated models far more than large ones, so an
//! anchor is only *accepted* when its captured statistics match what the anchor
//! is supposed to teach: a person present (or absent for `empty`), and the
//! expected stillness/motion. Failed anchors are re-prompted, not silently kept.
//!
//! Quality is measured against the ADR-135 empty-room baseline via
//! [`wifi_densepose_signal::BaselineCalibration::deviation`], whose
//! `CalibrationDeviationScore` gives a per-frame amplitude z-score (presence
//! strength) and a motion flag — exactly the two signals the gate needs.

use wifi_densepose_core::types::CsiFrame;
use wifi_densepose_signal::{BaselineCalibration, CalibrationDeviationScore};

use crate::anchor::{Anchor, AnchorLabel, AnchorQuality};

/// Thresholds for accepting an anchor.
#[derive(Debug, Clone, Copy)]
pub struct AnchorQualityGate {
    /// Minimum mean amplitude z-score to consider a person present.
    pub min_presence_z: f32,
    /// For `empty`: maximum mean z-score to consider the room truly empty.
    pub empty_max_z: f32,
    /// For "still" anchors: maximum motion-flag rate tolerated.
    pub max_still_motion: f32,
    /// For the "move" anchor: minimum motion-flag rate required.
    pub min_move_motion: f32,
    /// Minimum frames required to evaluate an anchor.
    pub min_frames: u32,
}

impl Default for AnchorQualityGate {
    fn default() -> Self {
        Self {
            min_presence_z: 1.5,
            empty_max_z: 1.0,
            max_still_motion: 0.6,
            min_move_motion: 0.3,
            min_frames: 60,
        }
    }
}

impl AnchorQualityGate {
    /// Evaluate accumulated stats for `label`, returning the quality verdict
    /// and (on rejection) a human-readable reason.
    pub fn evaluate(
        &self,
        label: AnchorLabel,
        presence_z: f32,
        motion_rate: f32,
        frames: u32,
    ) -> (AnchorQuality, Option<String>) {
        let mut reason: Option<String> = None;

        if frames < self.min_frames {
            reason = Some(format!(
                "only {frames} frames (need ≥{}); is the ESP32 streaming?",
                self.min_frames
            ));
        } else if label.expects_presence() {
            if presence_z < self.min_presence_z {
                reason = Some(format!(
                    "no person detected (presence_z {presence_z:.2} < {:.2}) — move closer / face the sensor",
                    self.min_presence_z
                ));
            } else if label.expects_still() && motion_rate > self.max_still_motion {
                reason = Some(format!(
                    "too much motion ({:.0}% > {:.0}%) for a still anchor — hold still",
                    motion_rate * 100.0,
                    self.max_still_motion * 100.0
                ));
            } else if !label.expects_still() && motion_rate < self.min_move_motion {
                reason = Some(format!(
                    "not enough motion ({:.0}% < {:.0}%) — move a bit more",
                    motion_rate * 100.0,
                    self.min_move_motion * 100.0
                ));
            }
        } else {
            // `empty` anchor: the room must actually be empty.
            if presence_z > self.empty_max_z {
                reason = Some(format!(
                    "room not empty (presence_z {presence_z:.2} > {:.2}) — clear the room",
                    self.empty_max_z
                ));
            }
        }

        let quality = AnchorQuality {
            presence_z,
            motion_rate,
            frames,
            accepted: reason.is_none(),
        };
        (quality, reason)
    }
}

/// Accumulates per-frame deviation statistics for a single anchor capture.
pub struct AnchorRecorder {
    label: AnchorLabel,
    z_sum: f64,
    motion_count: u32,
    frames: u32,
}

impl AnchorRecorder {
    /// Start recording the given anchor.
    pub fn new(label: AnchorLabel) -> Self {
        Self {
            label,
            z_sum: 0.0,
            motion_count: 0,
            frames: 0,
        }
    }

    /// The anchor being recorded.
    pub fn label(&self) -> AnchorLabel {
        self.label
    }

    /// Frames recorded so far.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Record a pre-computed deviation score (caller runs `baseline.deviation`).
    pub fn record_score(&mut self, score: &CalibrationDeviationScore) {
        self.z_sum += score.amplitude_z_median as f64;
        if score.motion_flagged {
            self.motion_count += 1;
        }
        self.frames += 1;
    }

    /// Convenience: record a CSI frame directly against a baseline.
    /// Frames that fail baseline geometry checks are skipped (not counted).
    pub fn record_frame(&mut self, baseline: &BaselineCalibration, frame: &CsiFrame) {
        if let Ok(score) = baseline.deviation(frame) {
            self.record_score(&score);
        }
    }

    /// Mean presence z-score over the capture.
    pub fn presence_z(&self) -> f32 {
        if self.frames == 0 {
            0.0
        } else {
            (self.z_sum / self.frames as f64) as f32
        }
    }

    /// Fraction of frames flagged as motion.
    pub fn motion_rate(&self) -> f32 {
        if self.frames == 0 {
            0.0
        } else {
            self.motion_count as f32 / self.frames as f32
        }
    }

    /// Evaluate the capture against the gate and produce an `Anchor` (accepted
    /// or not) plus a rejection reason.
    pub fn finalize(
        &self,
        gate: &AnchorQualityGate,
        at_unix_s: i64,
    ) -> (Anchor, Option<String>) {
        let (quality, reason) =
            gate.evaluate(self.label, self.presence_z(), self.motion_rate(), self.frames);
        (
            Anchor {
                label: self.label,
                captured_at_unix_s: at_unix_s,
                quality,
            },
            reason,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(z: f32, motion: bool) -> CalibrationDeviationScore {
        CalibrationDeviationScore {
            amplitude_z_median: z,
            amplitude_z_max: z + 1.0,
            phase_drift_median: 0.05,
            motion_flagged: motion,
        }
    }

    fn run(label: AnchorLabel, z: f32, motion: bool, n: u32) -> (Anchor, Option<String>) {
        let mut r = AnchorRecorder::new(label);
        for _ in 0..n {
            r.record_score(&score(z, motion));
        }
        r.finalize(&AnchorQualityGate::default(), 100)
    }

    #[test]
    fn still_anchor_with_present_still_person_accepts() {
        let (a, reason) = run(AnchorLabel::StandStill, 3.0, false, 400);
        assert!(a.quality.accepted, "reason: {reason:?}");
        assert!(reason.is_none());
    }

    #[test]
    fn still_anchor_rejects_when_no_presence() {
        let (a, reason) = run(AnchorLabel::Sit, 0.4, false, 400);
        assert!(!a.quality.accepted);
        assert!(reason.unwrap().contains("no person"));
    }

    #[test]
    fn still_anchor_rejects_on_motion() {
        let (a, reason) = run(AnchorLabel::LieDown, 3.0, true, 400);
        assert!(!a.quality.accepted);
        assert!(reason.unwrap().contains("motion"));
    }

    #[test]
    fn move_anchor_requires_motion() {
        let (still, r1) = run(AnchorLabel::SmallMove, 3.0, false, 400);
        assert!(!still.quality.accepted);
        assert!(r1.unwrap().contains("not enough motion"));
        let (moving, r2) = run(AnchorLabel::SmallMove, 3.0, true, 400);
        assert!(moving.quality.accepted, "reason: {r2:?}");
    }

    #[test]
    fn empty_anchor_rejects_when_occupied() {
        let (occupied, reason) = run(AnchorLabel::Empty, 3.0, true, 400);
        assert!(!occupied.quality.accepted);
        assert!(reason.unwrap().contains("not empty"));
        let (empty, _) = run(AnchorLabel::Empty, 0.3, false, 400);
        assert!(empty.quality.accepted);
    }

    #[test]
    fn too_few_frames_rejected() {
        let (a, reason) = run(AnchorLabel::Sit, 3.0, false, 10);
        assert!(!a.quality.accepted);
        assert!(reason.unwrap().contains("frames"));
    }
}
