//! Read-only clinical dashboard API (ADR-251 §2.2).
//!
//! Strictly observational: no route mutates stimulation state, widens an
//! envelope, or writes to the store. Claim discipline is inherited — the
//! acceptance payload carries the gate's `released_claim` verbatim (which is
//! `NO_CLAIM` for any program that has not passed), never a raw program claim.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::store::ClinicStore;

/// Shared, read-locked store handle.
pub type SharedStore = Arc<RwLock<ClinicStore>>;

/// The embedded single-file dashboard (no build step, no CDN — auditable by
/// reading one file).
pub const DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// Participant list row.
#[derive(Debug, Serialize)]
struct ParticipantRow {
    tag: String,
    sessions: usize,
    mean_entrainment: f64,
    safety_stops: usize,
    drift_flagged: bool,
}

/// Per-participant detail: response map + session trend.
#[derive(Debug, Serialize)]
struct ParticipantDetail {
    tag: String,
    /// `(frequency_hz, score)` points sorted by frequency — the personal
    /// response map rendered by the dashboard.
    frequency_curve: Vec<(f64, f64)>,
    sessions: Vec<SessionPoint>,
}

#[derive(Debug, Serialize)]
struct SessionPoint {
    frequency_hz: f64,
    entrainment_score: f64,
    comfort: f64,
    safety_pass: bool,
    session_hash: String,
    timestamp_ms: u64,
}

#[derive(Debug, Serialize)]
struct CohortView {
    clusters: Vec<Cluster>,
}

#[derive(Debug, Serialize)]
struct Cluster {
    members: Vec<String>,
}

/// Build the dashboard router over a shared store.
pub fn router(store: SharedStore) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/api/clinic/participants", get(participants))
        .route("/api/clinic/participants/:tag", get(participant_detail))
        .route("/api/clinic/cohort", get(cohort))
        .route("/api/clinic/acceptance", get(acceptance))
        .route("/api/clinic/integrity", get(integrity))
        .with_state(store)
}

async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn participants(State(store): State<SharedStore>) -> Json<Vec<ParticipantRow>> {
    let s = store.read().await;
    let rows = s
        .participant_tags()
        .into_iter()
        .map(|tag| {
            let sessions = s.sessions_for(&tag);
            let n = sessions.len();
            let mean = if n > 0 {
                sessions.iter().map(|x| x.entrainment_score).sum::<f64>() / n as f64
            } else {
                0.0
            };
            let stops = sessions.iter().filter(|x| !x.safety_pass).count();
            // Adverse flag from the stored vector (index 19 is sticky).
            let drift_flagged = s
                .profile_for(&tag)
                .map(|p| p.vector[19] >= 1.0)
                .unwrap_or(false);
            ParticipantRow {
                tag,
                sessions: n,
                mean_entrainment: mean,
                safety_stops: stops,
                drift_flagged,
            }
        })
        .collect();
    Json(rows)
}

async fn participant_detail(
    State(store): State<SharedStore>,
    Path(tag): Path<String>,
) -> Result<Json<ParticipantDetail>, StatusCode> {
    let s = store.read().await;
    let sessions = s.sessions_for(&tag);
    let profile = s.profile_for(&tag);
    if sessions.is_empty() && profile.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    // Frequency curve: prefer the stored profile's transferable map; fall back
    // to per-session (frequency, score) points.
    let mut frequency_curve: Vec<(f64, f64)> = match profile {
        Some(p) if !p.frequency_scores.is_empty() => p.frequency_scores.clone(),
        _ => sessions
            .iter()
            .map(|x| (x.frequency_hz, x.entrainment_score))
            .collect(),
    };
    frequency_curve.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(ParticipantDetail {
        tag,
        frequency_curve,
        sessions: sessions
            .iter()
            .map(|x| SessionPoint {
                frequency_hz: x.frequency_hz,
                entrainment_score: x.entrainment_score,
                comfort: x.comfort,
                safety_pass: x.safety_pass,
                session_hash: x.session_hash.clone(),
                timestamp_ms: x.timestamp_ms,
            })
            .collect(),
    }))
}

async fn cohort(State(store): State<SharedStore>) -> Json<CohortView> {
    let s = store.read().await;
    let profiles = s.profiles();
    let n = profiles.len();
    if n == 0 {
        return Json(CohortView { clusters: Vec::new() });
    }
    let k = 3.min(n);
    let assign = profiles.cluster(k, 10);
    let mut clusters: Vec<Cluster> = (0..k).map(|_| Cluster { members: Vec::new() }).collect();
    for (i, &c) in assign.iter().enumerate() {
        if let Some(p) = profiles.profile(i) {
            clusters[c].members.push(p.profile_tag.clone());
        }
    }
    clusters.retain(|c| !c.members.is_empty());
    Json(CohortView { clusters })
}

async fn acceptance(State(store): State<SharedStore>) -> impl IntoResponse {
    let s = store.read().await;
    // Serialize the stored summaries directly — `released_claim` is the gate's
    // output, recorded at evaluation time; this surface never reconstructs or
    // upgrades a claim.
    let list: Vec<_> = s.acceptance_reports().values().cloned().collect();
    Json(list)
}

async fn integrity(State(store): State<SharedStore>) -> impl IntoResponse {
    let s = store.read().await;
    Json(s.verify_chain())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{AcceptanceSummary, ClinicRecord, SessionSummary};
    use axum::body::Body;
    use axum::http::Request;
    use ruview_gamma::acceptance::NO_CLAIM;
    use ruview_gamma::ruvector::{AnonymizedProfile, VECTOR_DIM};
    use tower::ServiceExt;

    async fn body_json(res: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn seeded_store() -> SharedStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clinic.jsonl");
        let mut s = ClinicStore::open(&path).unwrap();
        let mut vector = [0.5; VECTOR_DIM];
        vector[11] = 39.0;
        s.append(ClinicRecord::Profile(AnonymizedProfile {
            profile_tag: "tag-a".into(),
            vector,
            frequency_scores: vec![(38.0, 0.5), (39.0, 0.8), (40.0, 0.6)],
        }))
        .unwrap();
        for (hz, score, pass) in [(38.0, 0.5, true), (39.0, 0.8, true), (40.0, 0.6, false)] {
            s.append(ClinicRecord::Session(SessionSummary {
                profile_tag: "tag-a".into(),
                program_id: "alzheimers-research".into(),
                frequency_hz: hz,
                entrainment_score: score,
                comfort: 0.85,
                safety_pass: pass,
                session_hash: "cd".repeat(32),
                timestamp_ms: 1,
            }))
            .unwrap();
        }
        // One passed and one withheld acceptance verdict.
        s.append(ClinicRecord::Acceptance(AcceptanceSummary {
            program_id: "attention-working-memory".into(),
            entrainment_gain: 0.3,
            safety_stop_rate: 0.0,
            mean_adherence: 0.95,
            repeatability_band_hz: 0.8,
            overall_pass: true,
            released_claim: "personalized frequency-response discovery".into(),
        }))
        .unwrap();
        s.append(ClinicRecord::Acceptance(AcceptanceSummary {
            program_id: "home-wellness".into(),
            entrainment_gain: 0.05,
            safety_stop_rate: 0.0,
            mean_adherence: 0.9,
            repeatability_band_hz: 3.0,
            overall_pass: false,
            released_claim: NO_CLAIM.into(),
        }))
        .unwrap();
        // Leak the tempdir so the file outlives the test router.
        std::mem::forget(dir);
        Arc::new(RwLock::new(s))
    }

    async fn get(router: &Router, uri: &str) -> axum::response::Response {
        router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn dashboard_html_served() {
        let r = router(seeded_store());
        let res = get(&r, "/").await;
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(html.contains("Clinical Dashboard"));
        assert!(html.contains("research use only"));
    }

    #[tokio::test]
    async fn participants_lists_sessions_and_stops() {
        let r = router(seeded_store());
        let v = body_json(get(&r, "/api/clinic/participants").await).await;
        assert_eq!(v[0]["tag"], "tag-a");
        assert_eq!(v[0]["sessions"], 3);
        assert_eq!(v[0]["safety_stops"], 1);
    }

    #[tokio::test]
    async fn participant_detail_serves_sorted_frequency_curve() {
        let r = router(seeded_store());
        let v = body_json(get(&r, "/api/clinic/participants/tag-a").await).await;
        let curve = v["frequency_curve"].as_array().unwrap();
        assert_eq!(curve.len(), 3);
        // Sorted ascending by frequency.
        assert!(curve[0][0].as_f64().unwrap() < curve[2][0].as_f64().unwrap());
        assert_eq!(v["sessions"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn unknown_participant_is_404() {
        let r = router(seeded_store());
        assert_eq!(get(&r, "/api/clinic/participants/nobody").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn acceptance_payload_uses_gated_claim() {
        let r = router(seeded_store());
        let v = body_json(get(&r, "/api/clinic/acceptance").await).await;
        let list = v.as_array().unwrap();
        assert_eq!(list.len(), 2);
        // The failed program surfaces NO_CLAIM verbatim — never its raw claim.
        let withheld = list.iter().find(|a| a["program_id"] == "home-wellness").unwrap();
        assert_eq!(withheld["overall_pass"], false);
        assert_eq!(withheld["released_claim"], NO_CLAIM);
        let passed = list.iter().find(|a| a["program_id"] == "attention-working-memory").unwrap();
        assert_eq!(passed["released_claim"], "personalized frequency-response discovery");
    }

    #[tokio::test]
    async fn cohort_and_integrity_endpoints_respond() {
        let r = router(seeded_store());
        let co = body_json(get(&r, "/api/clinic/cohort").await).await;
        assert!(co["clusters"].as_array().unwrap().len() >= 1);
        let integ = body_json(get(&r, "/api/clinic/integrity").await).await;
        assert_eq!(integ["valid"], true);
        assert_eq!(integ["records"], 6);
    }

    #[tokio::test]
    async fn surface_is_read_only_no_mutating_routes() {
        // POST to every route must not be routable (405/404), proving the
        // surface cannot actuate or write.
        let r = router(seeded_store());
        for uri in [
            "/",
            "/api/clinic/participants",
            "/api/clinic/participants/tag-a",
            "/api/clinic/cohort",
            "/api/clinic/acceptance",
            "/api/clinic/integrity",
        ] {
            let res = r
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                res.status() == StatusCode::METHOD_NOT_ALLOWED
                    || res.status() == StatusCode::NOT_FOUND,
                "POST {uri} unexpectedly routable: {}",
                res.status()
            );
        }
    }
}
