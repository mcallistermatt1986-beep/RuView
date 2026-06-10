//! Neuromodulation programs (ADR-250 extension: adaptive sensory
//! neuromodulation, not just Alzheimer's).
//!
//! The platform thesis: RuView turns the body into the feedback signal,
//! RuVector turns repeated sessions into a personal response map, the device is
//! the actuator, and RuFlo makes the loop governed and auditable. The *real*
//! product is a personal neural-rhythm optimization platform — and each use
//! case is a [`NeuroProgram`] bundling its own safety envelope, starting prior,
//! objective weighting, physiological-state gating, evidence level, and the
//! single claim it is allowed to make.
//!
//! **Claim discipline is structural:** a program's [`NeuroProgram::claim`] is
//! always an *optimization / monitoring* statement, never a disease-treatment
//! claim. The disease context lives only in [`EvidenceLevel`], and a claim is
//! only releasable once the program clears its acceptance gate
//! (`crate::acceptance`).

use crate::objective::ObjectiveWeights;
use crate::response::SleepState;
use crate::stimulus::{DutyCycle, Modality, SafetyEnvelope, StimulusParameters};

/// How well-supported a program's *disease/context* hypothesis is in the
/// literature (the user's opportunity map). This gates nothing by itself — it
/// is metadata a clinician/operator reads alongside the acceptance report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceLevel {
    /// Preclinical + early human (e.g. Alzheimer's gamma work).
    MediumPreclinicalEarlyHuman,
    /// Early human signals only (post-stroke, sleep, mood).
    EarlyHuman,
    /// Mixed / protocol-dependent (attention, working memory).
    Mixed,
    /// Speculative (home neuro-wellness).
    Speculative,
    /// Strong *infrastructure* opportunity (drug+device trial monitoring) —
    /// the strength is in measurement/governance, not a therapeutic claim.
    StrongInfrastructure,
}

impl EvidenceLevel {
    pub fn tag(self) -> &'static str {
        match self {
            EvidenceLevel::MediumPreclinicalEarlyHuman => "medium_preclinical_early_human",
            EvidenceLevel::EarlyHuman => "early_human",
            EvidenceLevel::Mixed => "mixed_protocol_dependent",
            EvidenceLevel::Speculative => "speculative",
            EvidenceLevel::StrongInfrastructure => "strong_infrastructure",
        }
    }
}

/// Time-of-day preference for a program (state-dependent entrainment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimePreference {
    Morning,
    Evening,
    /// Quiet wakefulness (the Alzheimer's/attention default).
    QuietWake,
    /// Pre-sleep / during sleep (the sleep program).
    PreSleepOrSleep,
    /// Any time.
    Any,
}

/// A named neuromodulation program: everything that distinguishes one use case
/// from another, in one value.
#[derive(Debug, Clone)]
pub struct NeuroProgram {
    /// Stable machine id (used in the session witness / provenance).
    pub id: &'static str,
    /// Human-readable name.
    pub display_name: &'static str,
    /// Literature support for the *context* hypothesis.
    pub evidence_level: EvidenceLevel,
    /// The ONLY claim this program may surface (an optimization/monitoring
    /// statement — never a disease-treatment claim).
    pub claim: &'static str,
    /// Per-program safety envelope (optimization happens only inside it).
    pub envelope: SafetyEnvelope,
    /// Evidence-based starting prior for this program.
    pub prior: StimulusParameters,
    /// Objective weighting tuned to the program's goal.
    pub weights: ObjectiveWeights,
    /// Physiological states in which a session is *protocol-eligible* (e.g. the
    /// sleep program permits `Asleep`; attention requires wakefulness).
    pub eligible_states: &'static [SleepState],
    /// When the program prefers to run.
    pub time_preference: TimePreference,
}

impl NeuroProgram {
    /// Whether a session in `state` fits this program's protocol. This is a
    /// *protocol-fit* gate (measured by acceptance), distinct from the hard
    /// *safety* gate (`SafetyEnvelope` / `SafetyMonitor`).
    pub fn state_eligible(&self, state: SleepState) -> bool {
        self.eligible_states.contains(&state)
    }

    // ---- The opportunity map (one constructor per use case) ----

    /// Alzheimer's research — adaptive entrainment + trial monitoring. Matches
    /// the original ADR-250 defaults (conservative envelope, 40 Hz prior,
    /// default weights), so the `RufloGovernor::enroll` witness is unchanged.
    pub fn alzheimers_research() -> Self {
        Self {
            id: "alzheimers-research",
            display_name: "Alzheimer's Research (adaptive entrainment + trial monitoring)",
            evidence_level: EvidenceLevel::MediumPreclinicalEarlyHuman,
            claim: "personalized entrainment optimization",
            envelope: SafetyEnvelope::conservative(),
            prior: StimulusParameters::prior(),
            weights: ObjectiveWeights::default(),
            eligible_states: &[SleepState::QuietWake, SleepState::Drowsy],
            time_preference: TimePreference::QuietWake,
        }
    }

    /// Post-stroke cognition — recovery state tracking. Comfort-leaning, gentle
    /// onset (ramped), short sessions; recovery populations tolerate less.
    pub fn post_stroke_cognition() -> Self {
        let mut prior = StimulusParameters::prior();
        prior.duty_cycle = DutyCycle::Ramped;
        prior.brightness_level = 0.25;
        prior.volume_level = 0.24;
        prior.duration_minutes = 8.0;
        let mut weights = ObjectiveWeights::default();
        weights.comfort = 0.20;
        weights.breathing_stability = 0.15;
        weights.gamma_gain = 0.25;
        weights.overstimulation = 0.15;
        Self {
            id: "post-stroke-cognition",
            display_name: "Post-Stroke Cognition (recovery state tracking)",
            evidence_level: EvidenceLevel::EarlyHuman,
            claim: "personalized entrainment optimization with recovery-state monitoring",
            envelope: SafetyEnvelope {
                brightness_cap: 0.32,
                volume_cap: 0.32,
                max_duration_minutes: 12.0,
                ..SafetyEnvelope::conservative()
            },
            prior,
            weights,
            eligible_states: &[SleepState::QuietWake, SleepState::Drowsy],
            time_preference: TimePreference::Morning,
        }
    }

    /// Sleep optimization — time stimulation to sleep state. Permits `Drowsy`/
    /// `Asleep`, lowest intensity caps (must not degrade sleep), weights calm
    /// physiology over raw gamma.
    pub fn sleep_optimization() -> Self {
        let mut prior = StimulusParameters::prior();
        prior.modality = Modality::Audio; // light flicker is disruptive at sleep
        prior.brightness_level = 0.0;
        prior.volume_level = 0.18;
        prior.duty_cycle = DutyCycle::Ramped;
        prior.duration_minutes = 15.0;
        let mut weights = ObjectiveWeights::default();
        weights.gamma_gain = 0.20;
        weights.phase_locking = 0.20;
        weights.breathing_stability = 0.25; // calm sleep physiology is the point
        weights.comfort = 0.15;
        weights.overstimulation = 0.20;
        Self {
            id: "sleep-optimization",
            display_name: "Sleep Optimization (state-timed gamma)",
            evidence_level: EvidenceLevel::EarlyHuman,
            claim: "sleep-state-timed entrainment optimization",
            envelope: SafetyEnvelope {
                brightness_cap: 0.10, // near-dark
                volume_cap: 0.25,
                max_duration_minutes: 30.0,
                ..SafetyEnvelope::conservative()
            },
            prior,
            weights,
            eligible_states: &[SleepState::Drowsy, SleepState::Asleep, SleepState::QuietWake],
            time_preference: TimePreference::PreSleepOrSleep,
        }
    }

    /// Attention & working memory — personal frequency discovery. Evidence is
    /// mixed/protocol-dependent, so this program leans hardest on *entrainment*
    /// terms (find the individual's responsive frequency) under wakefulness.
    pub fn attention_working_memory() -> Self {
        let mut weights = ObjectiveWeights::default();
        weights.gamma_gain = 0.35;
        weights.phase_locking = 0.30;
        weights.comfort = 0.10;
        Self {
            id: "attention-working-memory",
            display_name: "Attention & Working Memory (personal frequency discovery)",
            evidence_level: EvidenceLevel::Mixed,
            claim: "personalized frequency-response discovery",
            envelope: SafetyEnvelope::conservative(),
            prior: StimulusParameters::prior(),
            weights,
            eligible_states: &[SleepState::QuietWake, SleepState::Active],
            time_preference: TimePreference::QuietWake,
        }
    }

    /// Mood & arousal regulation — avoid overstimulation, tune the calming
    /// response. Lowest gamma weight, highest comfort + overstimulation penalty.
    pub fn mood_arousal() -> Self {
        let mut prior = StimulusParameters::prior();
        prior.brightness_level = 0.22;
        prior.volume_level = 0.22;
        prior.duty_cycle = DutyCycle::Ramped;
        let mut weights = ObjectiveWeights::default();
        weights.gamma_gain = 0.20;
        weights.phase_locking = 0.15;
        weights.comfort = 0.25;
        weights.breathing_stability = 0.20;
        weights.overstimulation = 0.20;
        Self {
            id: "mood-arousal",
            display_name: "Mood & Arousal Regulation (calming-response tuning)",
            evidence_level: EvidenceLevel::EarlyHuman,
            claim: "personalized calming-response optimization",
            envelope: SafetyEnvelope {
                brightness_cap: 0.30,
                volume_cap: 0.30,
                ..SafetyEnvelope::conservative()
            },
            prior,
            weights,
            eligible_states: &[SleepState::QuietWake, SleepState::Drowsy],
            time_preference: TimePreference::Evening,
        }
    }

    /// Home neuro-wellness — safe personalization without treatment claims. The
    /// most conservative envelope and the shortest sessions; speculative
    /// evidence, so the claim is explicitly wellness-only.
    pub fn home_wellness() -> Self {
        let mut prior = StimulusParameters::prior();
        prior.brightness_level = 0.20;
        prior.volume_level = 0.20;
        prior.duration_minutes = 6.0;
        Self {
            id: "home-wellness",
            display_name: "Home Neuro-Wellness (no treatment claim)",
            evidence_level: EvidenceLevel::Speculative,
            claim: "personal neural-rhythm wellness optimization",
            envelope: SafetyEnvelope {
                brightness_cap: 0.28,
                volume_cap: 0.28,
                max_duration_minutes: 10.0,
                ..SafetyEnvelope::conservative()
            },
            prior,
            weights: ObjectiveWeights::default(),
            eligible_states: &[SleepState::QuietWake],
            time_preference: TimePreference::Any,
        }
    }

    /// Drug-plus-device trial infrastructure — the strongest near-term use. The
    /// value is the *governed measurement layer* (RuView state + adherence,
    /// RuVector response curve, RuFlo protocol/safety/consent/sham log), so the
    /// claim is about a biomarker-correlated protocol layer, not therapy.
    pub fn trial_infrastructure() -> Self {
        Self {
            id: "trial-infrastructure",
            display_name: "Drug+Device Trial Infrastructure (governed protocol layer)",
            evidence_level: EvidenceLevel::StrongInfrastructure,
            claim: "governed, reproducible entrainment-protocol measurement",
            envelope: SafetyEnvelope::conservative(),
            prior: StimulusParameters::prior(),
            weights: ObjectiveWeights::default(),
            eligible_states: &[SleepState::QuietWake, SleepState::Drowsy],
            time_preference: TimePreference::Any,
        }
    }

    /// Every built-in program — for catalog UIs and the acceptance test matrix.
    pub fn catalog() -> Vec<NeuroProgram> {
        vec![
            Self::alzheimers_research(),
            Self::post_stroke_cognition(),
            Self::sleep_optimization(),
            Self::attention_working_memory(),
            Self::mood_arousal(),
            Self::home_wellness(),
            Self::trial_infrastructure(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_program_prior_is_inside_its_envelope() {
        for p in NeuroProgram::catalog() {
            assert!(
                p.envelope.contains(&p.prior),
                "program {} prior must be inside its envelope",
                p.id
            );
        }
    }

    #[test]
    fn no_program_claim_is_a_disease_treatment_claim() {
        let banned = ["treat", "cure", "alzheimer", "stroke recovery cure", "therapy for"];
        for p in NeuroProgram::catalog() {
            let claim = p.claim.to_lowercase();
            for b in banned {
                assert!(
                    !claim.contains(b),
                    "program {} claim '{}' contains banned term '{}'",
                    p.id,
                    p.claim,
                    b
                );
            }
        }
    }

    #[test]
    fn objective_weights_are_well_formed() {
        for p in NeuroProgram::catalog() {
            let w = &p.weights;
            for v in [
                w.gamma_gain,
                w.phase_locking,
                w.breathing_stability,
                w.adherence,
                w.comfort,
                w.motion_artifact,
                w.adverse_event_risk,
                w.overstimulation,
            ] {
                assert!((0.0..=1.0).contains(&v));
            }
        }
    }

    #[test]
    fn sleep_program_permits_asleep_others_do_not() {
        assert!(NeuroProgram::sleep_optimization().state_eligible(SleepState::Asleep));
        assert!(!NeuroProgram::attention_working_memory().state_eligible(SleepState::Asleep));
        assert!(!NeuroProgram::alzheimers_research().state_eligible(SleepState::Asleep));
    }

    #[test]
    fn sleep_program_caps_brightness_near_dark() {
        assert!(NeuroProgram::sleep_optimization().envelope.brightness_cap <= 0.10);
    }

    #[test]
    fn program_ids_are_unique() {
        let cat = NeuroProgram::catalog();
        let mut ids: Vec<&str> = cat.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn alzheimers_program_matches_adr250_defaults() {
        // The default-enroll path must be unchanged (witness stability).
        let p = NeuroProgram::alzheimers_research();
        assert_eq!(p.envelope, SafetyEnvelope::conservative());
        assert_eq!(p.prior, StimulusParameters::prior());
    }
}
