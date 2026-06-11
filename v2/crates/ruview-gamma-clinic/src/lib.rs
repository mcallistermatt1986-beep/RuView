//! # ruview-gamma-clinic — Clinical dashboard + persistent RuVector store (ADR-251)
//!
//! Read-only research/clinical instrumentation over the ADR-250 adaptive-gamma
//! platform: a **hash-chained JSONL store** (profiles, witnessed session
//! summaries, acceptance verdicts — any retroactive edit breaks the chain and
//! the store refuses to open) plus an **axum dashboard** (participant response
//! maps, session trends with safety-stop markers, cohort clusters, per-program
//! acceptance verdicts carrying only gate-released claims, and a live
//! chain-integrity badge).
//!
//! > **Not a medical device.** This surface can neither start stimulation nor
//! > widen a safety envelope — there are no mutating routes (tested). It
//! > renders what the ADR-250 acceptance gate released, nothing stronger.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use tokio::sync::RwLock;
//! use ruview_gamma_clinic::{store::ClinicStore, server::router};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Arc::new(RwLock::new(ClinicStore::open("clinic.jsonl")?));
//! let app = router(store);
//! let listener = tokio::net::TcpListener::bind("127.0.0.1:8090").await?;
//! axum::serve(listener, app).await?;
//! # Ok(())
//! # }
//! ```

pub mod server;
pub mod store;

use ruview_gamma::ruflo::RufloGovernor;
use store::{ClinicRecord, ClinicStore, SessionSummary, StoreError};

/// Ingest a governor's current state into the store: upsert the anonymized
/// profile and append any sessions not yet persisted (deduplicated by the
/// session witness hash). Returns how many new sessions were appended.
///
/// This is the bridge from the live ADR-250 loop to the durable clinic record:
/// call it after a session (or batch) completes.
///
/// # Errors
/// Propagates [`StoreError`] from the underlying append.
pub fn ingest_governor(
    store: &mut ClinicStore,
    gov: &RufloGovernor,
    program_id: &str,
) -> Result<usize, StoreError> {
    let profile = gov.export_anonymized_profile();
    let tag = profile.profile_tag.clone();
    store.append(ClinicRecord::Profile(profile))?;

    let known: std::collections::BTreeSet<String> = store
        .sessions_for(&tag)
        .iter()
        .map(|s| s.session_hash.clone())
        .collect();

    let mut appended = 0usize;
    for rec in gov.audit_log() {
        if known.contains(&rec.session_hash) {
            continue;
        }
        store.append(ClinicRecord::Session(SessionSummary {
            profile_tag: tag.clone(),
            program_id: program_id.to_string(),
            frequency_hz: rec.stimulus.frequency_hz,
            entrainment_score: rec.outcome.entrainment_score,
            comfort: rec.subjective.comfort,
            safety_pass: rec.outcome.safety_pass,
            session_hash: rec.session_hash.clone(),
            timestamp_ms: rec.timestamp_ms,
        }))?;
        appended += 1;
    }
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruview_gamma::response::RuViewState;
    use ruview_gamma::ruflo::Consent;
    use ruview_gamma::simulator::{LatentPerson, ResponseSimulator};
    use ruview_gamma::stimulus::SafetyEnvelope;

    /// End-to-end: a governed calibration run lands in the store with the
    /// pseudonymous tag, witnessed hashes, and an intact chain — and re-ingest
    /// is idempotent (witness-hash dedup).
    #[test]
    fn governor_ingest_roundtrip_and_idempotence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clinic.jsonl");
        let mut store = ClinicStore::open(&path).unwrap();

        let mut gov = RufloGovernor::enroll(
            "subject-secret-001",
            SafetyEnvelope::conservative(),
            &[],
            Consent::Granted,
        )
        .unwrap();
        let sim = ResponseSimulator::new(42);
        let latent = LatentPerson::from_id("subject-secret-001");
        gov.run_calibration(&sim, &latent, &RuViewState::calm_baseline(), 5.0, 0)
            .unwrap();

        let n = ingest_governor(&mut store, &gov, "alzheimers-research").unwrap();
        assert_eq!(n, 9); // the 36..44 Hz sweep

        // Pseudonymity: the person_id never appears anywhere in the file.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("subject-secret-001"));

        // Re-ingest appends nothing new (dedup by witness hash).
        let again = ingest_governor(&mut store, &gov, "alzheimers-research").unwrap();
        assert_eq!(again, 0);

        // Chain stays valid across reopen, with sessions queryable by tag.
        let reopened = ClinicStore::open(&path).unwrap();
        assert!(reopened.verify_chain().valid);
        let tags = reopened.participant_tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(reopened.sessions_for(&tags[0]).len(), 9);
    }
}
