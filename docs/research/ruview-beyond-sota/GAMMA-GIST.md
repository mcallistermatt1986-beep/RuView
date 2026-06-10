<!--
  Publication-ready GitHub Gist. Publish as a SECRET gist with your own token:

    gh gist create --desc "RuView Gamma — adaptive sensory neuromodulation" \
      docs/research/ruview-beyond-sota/GAMMA-GIST.md
    # (omit --public for a secret/unlisted gist; gh uses your stored PAT/token)

  Or with curl + a fine-grained PAT that has the "gists" scope:
    curl -H "Authorization: Bearer $GH_TOKEN" \
      -X POST https://api.github.com/gists \
      -d '{"public":false,"files":{"ruview-gamma.md":{"content":"...">}}}'

  Do NOT paste the token into this file or any committed file.
  SEO meta description (use as the gist description):
  "RuView Gamma: an open, governed engine for adaptive 40 Hz light-and-sound
   neuromodulation — personalized entrainment with proof discipline, built on
   RuView WiFi sensing and RuVector learning. Research platform, not a medical
   device."
-->

# RuView Gamma — Adaptive Sensory Neuromodulation, Done Honestly

> **TL;DR** — RuView Gamma is the open-source **control brain** for an adaptive
> light-and-sound (40 Hz "gamma") neuromodulation device. It personalizes the
> stimulation to each person, watches the body as feedback, learns what works,
> and **refuses to advertise any benefit it hasn't measured**. The most valuable
> thing here is not 40 Hz — it is a governed personalization engine that won't
> overpromise.
>
> **Not a medical device. Not medical advice.** It is a research and engineering
> platform. It makes no Alzheimer's, disease, or treatment claims.

**Keywords:** gamma entrainment, 40 Hz stimulation, GENUS, sensory
neuromodulation, WiFi sensing, adaptive personalization, Bayesian optimization,
Rust, ESP32, digital therapeutics infrastructure, RuView, RuVector.

**Projects:** [RuView](https://github.com/ruvnet/ruview) ·
RuVector (vector learning / response modeling) ·
Branch: `claude/ruview-beyond-sota-xgv8aq` ·
Crate: `v2/crates/ruview-gamma` · Firmware: `firmware/esp32-gamma-stim` ·
Decision record: `docs/adr/ADR-250-adaptive-gamma-entrainment.md`.

---

## 1. The easy introduction

Research from MIT and others (the "GENUS" line of work) found that sitting in
front of a light flickering ~40 times per second, with a matching pulsing
sound, can drive a brain rhythm called **gamma** — studied for Alzheimer's,
post-stroke recovery, sleep, focus, and mood.

Today's devices play a **fixed 40 Hz to everyone**. But brains differ: your best
frequency might be 38.5, 41.2, or somewhere else, and it changes with how calm,
tired, or restless you are. There's no off-the-shelf software that personalizes
this **safely and provably**.

RuView Gamma is that software. Four parts work together:

| Part | Role | Plain-English job |
|------|------|-------------------|
| **The device** | Actuator | Plays the light + sound |
| **RuView** | Sensing | Reads the body as feedback (breathing, stillness, restlessness) over WiFi — no camera, no wearable |
| **RuVector** | Learning | Builds a personal "response map" across sessions |
| **RuFlo** | Governance | Safety stops, tamper-evident audit log, and the claim boundary |

The thesis in one line: **RuView turns the body into the feedback signal,
RuVector turns repeated sessions into a personal response map, the device is the
actuator, and RuFlo makes the whole loop governed, measurable, and auditable.**

---

## 2. How it works

```
enroll (consent + epilepsy/photosensitivity screen)
  → start from 40 Hz prior
  → play a short calibration sweep (36–44 Hz)
  → RuView reads body state each session
  → score "safe entrainment" (not raw gamma)
  → RuVector updates the personal response map
  → Bayesian optimizer recommends the next best safe setting
  → every session is witness-hashed into a tamper-evident log
```

Key design choices that make it trustworthy:

- **40 Hz is a starting guess, not the answer.** A Gaussian-process optimizer
  searches the safe 36–44 Hz band for *your* peak — and proves it can recover a
  known peak within ±1 Hz in tests.
- **Safety is a hard gate, not a weighted preference.** A latched safety monitor
  stops on adverse symptoms, a stop request, or low sensor confidence — in about
  **9 nanoseconds** per check — and once it fires, the session **cannot silently
  resume**.
- **A compiled-in safety envelope** (36–44 Hz, capped brightness/volume/
  duration) bounds everything. The optimizer can never widen it.
- **Cross-person warm-start without identity.** A new user can be seeded from
  anonymized, one-way-hashed profiles of similar responders — but borrowed
  expectations are **down-weighted** and never counted as your measured data.
- **Tamper-evident proof.** Every session produces a SHA-256 witness over
  exactly what was played and sensed. Re-running the same inputs reproduces the
  identical hash — a regulator, clinician, or trial auditor can verify nothing
  was fudged. The pinned reference witness is `13cb164c…`.

### The hard claim gate (the important part)

A program may surface a benefit claim **only** if all four pass:

```
claim_allowed = entrainment_pass AND safety_pass
             AND adherence_pass  AND repeatability_pass
```

Anything less returns `research use only — no claim`. The marketing claim is
literally unreadable in the code except through this gate.

### It's a platform, not one gadget

Seven programs ship, each with its own safety envelope, objective, state-gating,
evidence level, and a single non-disease claim:

| Program | Evidence | What it tunes for |
|---------|----------|-------------------|
| Alzheimer's research | medium preclinical / early human | entrainment + trial monitoring |
| Post-stroke cognition | early human | gentle, recovery-state tracking |
| Sleep optimization | early/plausible | audio-only, near-dark, timed to sleep state |
| Attention / working memory | mixed | personal frequency discovery |
| Mood / arousal | early human | calming response, avoid overstimulation |
| Home wellness | speculative | safe personalization, no treatment claim |
| Drug + device trial infra | strong (as infrastructure) | governed, reproducible measurement |

---

## 3. Research supporting it (and its honest limits)

- **Preclinical (strongest):** a 2024 *Nature* paper showed 40 Hz multisensory
  stimulation promoted cerebrospinal-fluid influx and amyloid clearance via the
  glymphatic system in Alzheimer's-model mice; blocking that clearance abolished
  the effect.
- **Early human:** a 2022 study found 40 Hz sensory stimulation feasible and
  well-tolerated in mild Alzheimer's, with exploratory signals on structure,
  connectivity, sleep, and memory. A small 2025 two-year pilot reported safety
  and feasibility, but the sample was tiny and not definitive.
- **Frequency is not one-size-fits-all:** a 2025 *PLOS One* study re-evaluated
  gamma frequency across 36–44 Hz — direct motivation for *measuring* the
  individual's frequency rather than assuming 40 Hz.
- **Adjacent areas (early/mixed):** post-stroke cognition, sleep (40 Hz evoked
  without degrading sleep), attention/working-memory (mixed, protocol-dependent),
  and mood/arousal.

**Honest limits, encoded as non-goals:** RF sensing does not measure amyloid;
personalized frequency improving clinical outcomes is unproven; consumer use
without screening is not safe; 40 Hz is not always optimal. The software makes
none of these claims.

---

## 4. How to use it

### Run the governed engine (Rust)

```bash
git clone https://github.com/ruvnet/ruview
cd ruview && git checkout claude/ruview-beyond-sota-xgv8aq
cd v2
cargo test -p ruview-gamma --no-default-features      # 97 tests + 1 doctest
cargo bench -p ruview-gamma --no-default-features      # criterion micro-benchmarks
```

```rust
use ruview_gamma::{
    ruflo::{Consent, RufloGovernor},
    program::NeuroProgram,
    response::RuViewState,
    simulator::{LatentPerson, ResponseSimulator},
    stimulus::StimulusParameters,
};

let mut gov = RufloGovernor::enroll_program(
    "subject-001", NeuroProgram::sleep_optimization(), &[], Consent::Granted,
).expect("cleared to participate");

let sim = ResponseSimulator::new(42);          // deterministic stand-in for hardware
let latent = LatentPerson::from_id("subject-001");
let state = RuViewState::calm_baseline();
gov.run_calibration(&sim, &latent, &state, 5.0, 0).unwrap();

let rec = gov.recommend(&gov.prior());          // always inside the safety envelope
```

### Run the device (ESP32)

```bash
cd firmware/esp32-gamma-stim
# Host-side safety-core tests — no hardware, no ESP-IDF:
gcc -Wall -Wextra -Werror -O2 -I main tests/test_stim_core.c main/stim_core.c -o /tmp/t && /tmp/t
# On hardware (ESP-IDF v5.2+):
idf.py set-target esp32s3 && idf.py build flash monitor
```

Serial protocol (frequency in millihertz, so 40.0 Hz = `40000`):

```
START 40000 30 28 600   # 40 Hz, 30% brightness, 28% volume, 10 min
STOP | STATUS | UNLOCK | VERSION
```

### Benchmarks (indicative)

| Path | Time | Role |
|------|------|------|
| Safety tick | ~8 ns | real-time stop path |
| Recommendation | ~15 µs | per-session decision |
| Cohort kNN (500 profiles) | ~15 µs | warm-start matching |
| Calibration sweep | ~115 µs | setup/tuning |
| Full acceptance grading | ~425 µs | enrollment only |

---

## 5. Cautions (read these)

- ⚠️ **Flicker can trigger seizures.** Epilepsy and photosensitivity are **hard
  exclusions** in software; a hardware e-stop is mandatory for any human-facing
  run. Migraine/psychiatric instability/implanted neuro devices require
  clinical supervision.
- ⚠️ **Not a treatment.** No Alzheimer's/disease/efficacy claim. The only
  product claim is "personalized entrainment optimization," and even that is
  gated behind measured entrainment + safety + adherence + repeatability.
- ⚠️ **No hardware actuation in the core crate.** The Rust crate is a validated
  decision/safety/learning/audit engine tested against a simulator; the ESP32
  firmware is the actuator. Real EEG validation is a separate, deferred step.
- ⚠️ **Optical safety is the integrator's job.** The firmware caps PWM duty, but
  absolute luminance/eye-safety belongs to the LED driver and optical design.
- ⚠️ **Research/IRB context required** for any study with human subjects,
  especially clinical populations.

---

## 6. Advanced usages

- **Drug + device trials (strongest near-term use):** use RuFlo as the governed
  measurement layer — consent, inclusion/exclusion, **sham/blinding**, per-session
  witness hashes, and clinician export — to make *someone else's* therapy trial
  auditable and reproducible. The value is the instrument, not a therapy claim.
- **Sham-controlled studies:** `TrialMode::Sham` logs the participant-facing
  protocol while delivering no entrainment, for blinded arms.
- **Cohort transfer learning:** export anonymized response profiles and
  warm-start new participants via RuVector kNN — privacy-preserving, one-way
  hashed, never identity-bearing.
- **Drift-triggered recalibration:** a Welford-centroid drift detector flags when
  a person's physiology has shifted enough to warrant re-running calibration.
- **Hardware-in-the-loop acceptance:** capture LED frequency, A/V sync, and stop
  latency on the bench and grade them with `hil::verify_hil` against fixed
  targets (±0.1 Hz, <5 ms, <100 ms, 100% hash reproducibility, ≥20% EEG lift).
- **Edge deployment:** the engine is dependency-light and deterministic; an HNSW
  (RuVector) backend drops in for cohort search past ~10⁵ profiles.

---

## 7. Credits

- **RuView** — WiFi/RF human sensing platform that supplies the passive body
  feedback signal. https://github.com/ruvnet/ruview
- **RuVector** — vector learning / response-curve modeling (cohort warm-start,
  drift detection, clustering; HNSW-ready).
- **RuFlo** — governance, audit, consent, and protocol execution layer.
- Built by the RuView contributors on branch
  `claude/ruview-beyond-sota-xgv8aq`. Decision record: ADR-250.
- Scientific inspiration: the GENUS / 40 Hz gamma-entrainment research community
  (MIT Tsai/Boyden labs and others). This project implements *engineering and
  governance*, not their clinical findings.

---

## 8. FAQ (SEO)

**What is gamma entrainment?** Driving ~40 Hz brain rhythms with rhythmic light
and/or sound. RuView Gamma personalizes the exact frequency per person.

**Is RuView Gamma a medical device?** No. It is an open research and engineering
platform that makes no treatment or disease claims.

**Does it cure or treat Alzheimer's?** No. It optimizes and audits stimulation
protocols; clinical outcomes are explicitly out of scope.

**Can I run it on an ESP32?** Yes — `firmware/esp32-gamma-stim` drives the LED +
audio flicker with a hardware emergency stop and a compiled-in safety envelope.

**Why is it "honest"?** A program's benefit claim is unreadable in code until it
passes measured entrainment, safety, adherence, and repeatability.

**License / source:** see the RuView repository, branch
`claude/ruview-beyond-sota-xgv8aq`.

---

*RuView Gamma — personalized neural-rhythm optimization with tamper-evident
proof. Not a medical claim. Not a consumer miracle device. A tested, safety-first
engine ready for hardware, EEG validation, and serious clinical research.*
