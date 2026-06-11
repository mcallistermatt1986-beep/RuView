# ADR-251: Clinical Dashboard + Persistent RuVector Store for Adaptive Gamma

| Field | Value |
|-------|-------|
| **Status** | Proposed |
| **Date** | 2026-06-11 |
| **Owner** | RuView, RuVector, RuFlo clinical systems |
| **Decision type** | Architecture, clinical tooling |
| **Relates to** | ADR-250 (Adaptive Gamma Entrainment — platform, programs, acceptance gate) |
| **Codebase target** | `v2/crates/ruview-gamma-clinic` (this ADR's implementation) |

> **Not a medical device.** Read-only research/clinical *instrumentation* over
> the ADR-250 platform. It surfaces only gated claims and witnessed records; it
> can neither start stimulation nor widen any safety envelope.

## 1. Context

ADR-250 shipped the governed adaptive-neuromodulation engine: per-program
safety envelopes, the RuVector self-learning layer (anonymized profiles, kNN
warm-start, drift detection, clustering), witnessed session records, and the
executable acceptance gate. Two operational gaps remain for clinical use:

1. **No durable store.** `ProfileStore` and the governor's audit log are
   in-memory; a clinic restart loses cohort memory, and there is no
   tamper-evident persistence matching the platform's proof discipline.
2. **No clinician surface.** RuFlo's clinician export exists as a struct
   (`ClinicianReport`), but there is no way for a clinician/trial monitor to
   *see* a participant's frequency-response curve, session trend, safety
   events, drift status, or a program's acceptance verdict.

## 2. Decision

Build `ruview-gamma-clinic`, a separate crate (keeping `ruview-gamma` a
dependency-light deterministic leaf) with two components:

### 2.1 Persistent RuVector store (`store.rs`)

Append-only JSON-lines file per clinic, holding three record kinds —
anonymized profiles, witnessed session summaries, and acceptance reports —
each line **hash-chained** (`entry_hash = SHA-256(prev_hash ‖ canonical_json)`)
so any retroactive edit breaks the chain (`verify_chain()`); the RuVector
in-memory layer (`ProfileStore` kNN, clustering) is rebuilt from the file at
open. Pseudonymity is preserved: records carry only the one-way profile tags
from ADR-250 §10.

### 2.2 Read-only clinical dashboard (`server.rs` + embedded `dashboard.html`)

Axum surface, **strictly read-only** (no POST mutates stimulation state):

| Route | Payload |
|-------|---------|
| `GET /` | embedded single-file HTML dashboard (no build step; SVG charts) |
| `GET /api/clinic/participants` | tag, session count, mean entrainment, safety stops, adverse flag, drift status |
| `GET /api/clinic/participants/{tag}` | response vector, frequency→score curve, session trend |
| `GET /api/clinic/cohort` | deterministic k-means clusters over the stored profiles |
| `GET /api/clinic/acceptance` | per-program acceptance reports with the **gated** claim |
| `GET /api/clinic/integrity` | hash-chain verification result + record count |

**Claim discipline is inherited, not re-implemented:** acceptance payloads
embed `AcceptanceReport::released_claim` (the gate's output), never the
program's raw claim. The dashboard renders what the gate released — nothing
stronger.

### 2.3 Visualization (embedded, dependency-free)

One static HTML file (`include_str!`) with vanilla JS + inline SVG:
participant list → per-participant frequency-response curve (the personal
response map), entrainment/comfort session trend, safety-event markers, cohort
cluster table, and an integrity badge (green only when `verify_chain` passes).
No JS framework, no CDN, no build step — auditable by reading one file.

## 3. Consequences

- Clinic restarts no longer lose cohort memory; warm-start works across runs.
- Tampering with stored records is detectable by one endpoint call.
- A clinician can inspect a trial without shell access; the surface cannot
  actuate anything.
- The store is JSONL, not a database server: at research-cohort scale this is
  deliberate (greppable, diffable, witness-friendly). An HNSW/ruvector-crate
  backend remains the drop-in path past ~10⁵ profiles (ADR-250 §10).

## 4. Acceptance criteria (tested)

| Criterion | Test |
|-----------|------|
| Store round-trips all three record kinds across reopen | `store::tests` |
| Hash chain detects any line edit/deletion/reorder | `tampered_chain_is_detected` |
| kNN warm-start works from a reloaded store | `knn_survives_reload` |
| Every API route serves and is read-only | `server::tests` (oneshot) |
| Acceptance payload carries only the gated claim | `acceptance_payload_uses_gated_claim` |
| Dashboard HTML embeds and serves | `dashboard_html_served` |
