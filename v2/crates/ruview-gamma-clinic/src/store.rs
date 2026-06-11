//! Persistent, hash-chained RuVector store (ADR-251 §2.1).
//!
//! Append-only JSON-lines file holding three record kinds — anonymized
//! profiles, witnessed session summaries, and acceptance reports. Every line
//! is hash-chained: `entry_hash = SHA-256(prev_hash ‖ canonical_record_json)`,
//! so any retroactive edit, deletion, or reorder breaks [`ClinicStore::verify_chain`].
//! The RuVector in-memory layer (kNN, clustering) is rebuilt from the file on
//! [`ClinicStore::open`], so cohort warm-start survives restarts.
//!
//! Pseudonymity: records carry only the one-way profile tags from ADR-250 §10
//! — never a `person_id`, never raw sensor data.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ruview_gamma::ruvector::{AnonymizedProfile, ProfileStore};

/// Store errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A line failed to parse.
    #[error("corrupt record at line {line}: {reason}")]
    Corrupt { line: usize, reason: String },
}

/// One witnessed session summary, as persisted for the dashboard. A projection
/// of `ruview_gamma::session::SessionRecord` keyed by the one-way profile tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// One-way profile tag (never a person_id).
    pub profile_tag: String,
    /// Program the session ran under.
    pub program_id: String,
    /// Stimulation frequency (Hz).
    pub frequency_hz: f64,
    /// Safe-entrainment score for the session.
    pub entrainment_score: f64,
    /// Participant comfort `[0,1]`.
    pub comfort: f64,
    /// Whether the session passed without a safety stop.
    pub safety_pass: bool,
    /// The session's witness hash (hex SHA-256 from the RuFlo builder).
    pub session_hash: String,
    /// Caller-supplied epoch milliseconds.
    pub timestamp_ms: u64,
}

/// One persisted acceptance verdict (the gate's output, never the raw claim).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceSummary {
    /// Program graded.
    pub program_id: String,
    /// Measured entrainment gain vs the fixed prior.
    pub entrainment_gain: f64,
    /// Measured safety-stop rate.
    pub safety_stop_rate: f64,
    /// Measured mean adherence.
    pub mean_adherence: f64,
    /// Optimal-frequency spread across repeats (Hz).
    pub repeatability_band_hz: f64,
    /// Whether all four criteria passed.
    pub overall_pass: bool,
    /// The claim **as released by the gate** (`NO_CLAIM` on failure).
    pub released_claim: String,
}

/// A store record: exactly one of the three kinds per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClinicRecord {
    /// Anonymized responder profile (upserted by tag on load).
    Profile(AnonymizedProfile),
    /// Witnessed session summary.
    Session(SessionSummary),
    /// Acceptance verdict for a program.
    Acceptance(AcceptanceSummary),
}

/// One persisted line: the record plus its chain hash.
///
/// `record` is kept as a [`serde_json::value::RawValue`] so the chain hashes
/// the **exact bytes on disk**. Re-serializing a parsed record is not
/// byte-stable: serde_json's default float parsing is fast-but-lossy (±1 ulp;
/// exact parsing is behind its `float_roundtrip` feature), so
/// `to_string(from_str(x))` can differ from `x` for long float literals —
/// hash-by-reserialization would self-corrupt.
#[derive(Debug, Serialize, Deserialize)]
struct ChainedLine {
    record: Box<serde_json::value::RawValue>,
    /// hex SHA-256(prev_hash ‖ raw record json bytes)
    entry_hash: String,
}

/// Result of an integrity check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChainStatus {
    /// Whether every entry hash verified.
    pub valid: bool,
    /// Number of records in the chain.
    pub records: usize,
    /// First broken line (1-based), if any.
    pub broken_at: Option<usize>,
}

/// The persistent clinic store: hash-chained JSONL on disk + the RuVector
/// in-memory layer rebuilt on open.
pub struct ClinicStore {
    path: PathBuf,
    /// Last entry hash (hex) — the chain head.
    head: String,
    /// RuVector layer over the loaded profiles (kNN, clustering).
    profiles: ProfileStore,
    /// Session summaries by profile tag, in append order.
    sessions: BTreeMap<String, Vec<SessionSummary>>,
    /// Latest acceptance verdict per program.
    acceptance: BTreeMap<String, AcceptanceSummary>,
}

/// Chain-genesis constant (the `prev_hash` of the first record).
const GENESIS: &str = "ruview-gamma-clinic-genesis-v1";

fn entry_hash(prev: &str, record_json: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(record_json.as_bytes());
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl ClinicStore {
    /// Open (or create) a store at `path`, replaying and verifying every line.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] if a line fails to parse or breaks the chain —
    /// fail closed: a tampered store refuses to open rather than silently
    /// serving doctored data.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let mut store = Self {
            path: path.clone(),
            head: GENESIS.to_string(),
            profiles: ProfileStore::new(),
            sessions: BTreeMap::new(),
            acceptance: BTreeMap::new(),
        };
        if !path.exists() {
            return Ok(store);
        }
        let file = File::open(&path)?;
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let chained: ChainedLine =
                serde_json::from_str(&line).map_err(|e| StoreError::Corrupt {
                    line: i + 1,
                    reason: e.to_string(),
                })?;
            // Hash the exact raw bytes from disk — never a re-serialization.
            let expect = entry_hash(&store.head, chained.record.get());
            if expect != chained.entry_hash {
                return Err(StoreError::Corrupt {
                    line: i + 1,
                    reason: "hash chain broken".into(),
                });
            }
            let record: ClinicRecord =
                serde_json::from_str(chained.record.get()).map_err(|e| StoreError::Corrupt {
                    line: i + 1,
                    reason: e.to_string(),
                })?;
            store.head = chained.entry_hash;
            store.apply(record);
        }
        Ok(store)
    }

    /// Apply a record to the in-memory views.
    fn apply(&mut self, record: ClinicRecord) {
        match record {
            ClinicRecord::Profile(p) => self.profiles.upsert(p),
            ClinicRecord::Session(s) => {
                self.sessions.entry(s.profile_tag.clone()).or_default().push(s)
            }
            ClinicRecord::Acceptance(a) => {
                self.acceptance.insert(a.program_id.clone(), a);
            }
        }
    }

    /// Append a record: chain-hash its exact serialized bytes, write the line,
    /// update memory.
    pub fn append(&mut self, record: ClinicRecord) -> Result<(), StoreError> {
        let record_json = serde_json::to_string(&record)
            .map_err(|e| StoreError::Corrupt { line: 0, reason: e.to_string() })?;
        let hash = entry_hash(&self.head, &record_json);
        let raw = serde_json::value::RawValue::from_string(record_json)
            .map_err(|e| StoreError::Corrupt { line: 0, reason: e.to_string() })?;
        let line = serde_json::to_string(&ChainedLine {
            record: raw,
            entry_hash: hash.clone(),
        })
        .map_err(|e| StoreError::Corrupt { line: 0, reason: e.to_string() })?;
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        self.head = hash;
        self.apply(record);
        Ok(())
    }

    /// Re-read the file from disk and verify the whole chain (tamper check).
    pub fn verify_chain(&self) -> ChainStatus {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => {
                return ChainStatus { valid: true, records: 0, broken_at: None };
            }
        };
        let mut prev = GENESIS.to_string();
        let mut n = 0usize;
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let Ok(line) = line else {
                return ChainStatus { valid: false, records: n, broken_at: Some(i + 1) };
            };
            if line.trim().is_empty() {
                continue;
            }
            let parsed: Result<ChainedLine, _> = serde_json::from_str(&line);
            let Ok(chained) = parsed else {
                return ChainStatus { valid: false, records: n, broken_at: Some(i + 1) };
            };
            if entry_hash(&prev, chained.record.get()) != chained.entry_hash {
                return ChainStatus { valid: false, records: n, broken_at: Some(i + 1) };
            }
            prev = chained.entry_hash;
            n += 1;
        }
        ChainStatus { valid: true, records: n, broken_at: None }
    }

    /// The RuVector layer over loaded profiles (kNN / warm-start / clustering).
    pub fn profiles(&self) -> &ProfileStore {
        &self.profiles
    }

    /// Sessions for one profile tag, in append order.
    pub fn sessions_for(&self, tag: &str) -> &[SessionSummary] {
        self.sessions.get(tag).map(Vec::as_slice).unwrap_or(&[])
    }

    /// All profile tags with at least one session or profile, sorted.
    pub fn participant_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.sessions.keys().cloned().collect();
        for i in 0..self.profiles.len() {
            if let Some(p) = self.profiles.profile(i) {
                if !tags.contains(&p.profile_tag) {
                    tags.push(p.profile_tag.clone());
                }
            }
        }
        tags.sort();
        tags
    }

    /// The stored profile for a tag, if any.
    pub fn profile_for(&self, tag: &str) -> Option<&AnonymizedProfile> {
        (0..self.profiles.len())
            .filter_map(|i| self.profiles.profile(i))
            .find(|p| p.profile_tag == tag)
    }

    /// Latest acceptance verdicts, keyed by program id.
    pub fn acceptance_reports(&self) -> &BTreeMap<String, AcceptanceSummary> {
        &self.acceptance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruview_gamma::ruvector::VECTOR_DIM;

    fn profile(tag: &str, peak: f64) -> AnonymizedProfile {
        let mut vector = [0.5; VECTOR_DIM];
        vector[5] = 13.0;
        vector[11] = peak;
        AnonymizedProfile {
            profile_tag: tag.into(),
            vector,
            frequency_scores: vec![(peak - 1.0, 0.5), (peak, 0.8), (peak + 1.0, 0.5)],
        }
    }

    fn session(tag: &str, hz: f64, score: f64) -> SessionSummary {
        SessionSummary {
            profile_tag: tag.into(),
            program_id: "alzheimers-research".into(),
            frequency_hz: hz,
            entrainment_score: score,
            comfort: 0.9,
            safety_pass: true,
            session_hash: "ab".repeat(32),
            timestamp_ms: 1_700_000_000_000,
        }
    }

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clinic.jsonl");
        (dir, path)
    }

    #[test]
    fn roundtrips_all_record_kinds_across_reopen() {
        let (_d, path) = tmp();
        {
            let mut s = ClinicStore::open(&path).unwrap();
            s.append(ClinicRecord::Profile(profile("tag-a", 39.0))).unwrap();
            s.append(ClinicRecord::Session(session("tag-a", 39.0, 0.7))).unwrap();
            s.append(ClinicRecord::Session(session("tag-a", 39.5, 0.75))).unwrap();
            s.append(ClinicRecord::Acceptance(AcceptanceSummary {
                program_id: "sleep-optimization".into(),
                entrainment_gain: 0.25,
                safety_stop_rate: 0.0,
                mean_adherence: 0.95,
                repeatability_band_hz: 1.0,
                overall_pass: true,
                released_claim: "sleep-state-timed entrainment optimization".into(),
            }))
            .unwrap();
        }
        let s = ClinicStore::open(&path).unwrap();
        assert_eq!(s.participant_tags(), vec!["tag-a".to_string()]);
        assert_eq!(s.sessions_for("tag-a").len(), 2);
        assert_eq!(s.profile_for("tag-a").unwrap().frequency_scores.len(), 3);
        assert!(s.acceptance_reports().contains_key("sleep-optimization"));
        let st = s.verify_chain();
        assert!(st.valid);
        assert_eq!(st.records, 4);
    }

    #[test]
    fn tampered_chain_is_detected_and_refuses_open() {
        let (_d, path) = tmp();
        {
            let mut s = ClinicStore::open(&path).unwrap();
            s.append(ClinicRecord::Session(session("tag-a", 40.0, 0.6))).unwrap();
            s.append(ClinicRecord::Session(session("tag-a", 41.0, 0.7))).unwrap();
        }
        // Doctor the first line's score 0.6 -> 0.9 (a retroactive edit).
        let text = std::fs::read_to_string(&path).unwrap();
        let doctored = text.replacen("0.6", "0.9", 1);
        assert_ne!(text, doctored);
        std::fs::write(&path, doctored).unwrap();
        // Open fails closed…
        assert!(matches!(
            ClinicStore::open(&path),
            Err(StoreError::Corrupt { line: 1, .. })
        ));
    }

    #[test]
    fn deleting_a_line_breaks_the_chain() {
        let (_d, path) = tmp();
        {
            let mut s = ClinicStore::open(&path).unwrap();
            s.append(ClinicRecord::Session(session("a", 40.0, 0.5))).unwrap();
            s.append(ClinicRecord::Session(session("a", 41.0, 0.6))).unwrap();
            s.append(ClinicRecord::Session(session("a", 42.0, 0.7))).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let pruned: Vec<&str> = text.lines().enumerate().filter(|(i, _)| *i != 1).map(|(_, l)| l).collect();
        std::fs::write(&path, pruned.join("\n")).unwrap();
        assert!(ClinicStore::open(&path).is_err());
    }

    #[test]
    fn knn_survives_reload() {
        let (_d, path) = tmp();
        {
            let mut s = ClinicStore::open(&path).unwrap();
            s.append(ClinicRecord::Profile(profile("lo", 37.0))).unwrap();
            s.append(ClinicRecord::Profile(profile("hi", 43.0))).unwrap();
        }
        let s = ClinicStore::open(&path).unwrap();
        let mut q = [0.5; VECTOR_DIM];
        q[5] = 13.0;
        q[11] = 37.0;
        let nn = s.profiles().k_nearest(&q, 1);
        assert_eq!(s.profiles().profile(nn[0].0).unwrap().profile_tag, "lo");
        // Warm-start priors are constructible from the reloaded store.
        assert!(!s.profiles().warm_start_prior(&q, 2, 1e-4).is_empty());
    }

    #[test]
    fn empty_store_is_valid() {
        let (_d, path) = tmp();
        let s = ClinicStore::open(&path).unwrap();
        let st = s.verify_chain();
        assert!(st.valid);
        assert_eq!(st.records, 0);
    }
}
