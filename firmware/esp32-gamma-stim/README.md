# esp32-gamma-stim — ESP32 gamma stimulation actuator (ADR-250 §21 M2)

The **device harness** for `ruview-gamma`: an ESP32 that drives a light + sound
flicker at a commanded frequency, gated by a hardware emergency stop, with a
compiled-in safety envelope that mirrors `SafetyEnvelope::conservative()` in the
Rust crate. This is the actuator the `hil::verify_hil` contract grades.

> **Not a medical device.** Research/engineering harness. The host
> (`ruview-gamma`) decides *what* to play and never claims a therapeutic effect;
> this firmware only plays it safely and reports exactly what it did.

## Design: safety core vs hardware binding

| File | Role | Tested |
|------|------|--------|
| `main/stim_core.{h,c}` | Pure C safety core: envelope validation, START/STOP/e-stop **latched** state machine, exact integer timing math, line protocol parser. No ESP-IDF deps. | `tests/test_stim_core.c` on the host (gcc), 15 tests |
| `main/main.c` | ESP-IDF binding: GPTimer ISR toggling the **gate GPIO** (envelope + hard off path), LEDC PWM carriers (LED + audio, task-context only), sync GPIO, e-stop ISR, dead-man watchdog, USB-CDC console. Only moves registers. | on hardware (HIL) |

Every safety decision lives in the host-tested core — **defense in depth**: the
Rust host gates the stimulus *and* the device gates it again independently, so a
buggy or compromised host still cannot command an out-of-envelope output.

## Output gate wiring (hard off path) — REQUIRED

The LEDC driver APIs are **not ISR-safe** (non-IRAM, non-ISR internal locking),
so the firmware never touches LEDC from an interrupt. Instead the emitted
output is the **hardware AND** of two signals:

1. **LEDC carrier** (LED 19.5 kHz PWM / audio tone) at a *constant* duty for
   the whole session, programmed from task context at START.
2. **Gate GPIO** (`GAMMA_STIM_GATE_GPIO`, default 10) — a plain GPIO toggled
   by the half-period ISR with a direct register write. It delivers the
   36–44 Hz envelope *and* is the hard real-time off path: the e-stop ISR and
   the dead-man watchdog force it low in microseconds.

Wiring requirements:

- Route the gate pin to the **enable input of the LED driver and the audio
  amplifier** (or a series MOSFET that interrupts both). Output must be
  physically impossible unless `gate == high` *and* the carrier is running.
- Add an **external pulldown (~10 kΩ) on the gate pin** so the drivers stay
  disabled during the boot/reset window while the pin floats, and on firmware
  crash/brownout.
- Avoid strapping pins for the gate (e.g. GPIO8/GPIO9 on ESP32-C6).

Without this wiring the LED pin would carry the bare carrier for the whole
session (no envelope) — the gate is not optional.

## Independent dead-man watchdog

The session-duration cap is enforced by the same GPTimer ISR that toggles the
gate, so a stalled/killed ISR could otherwise leave the envelope ON. A separate
250 ms FreeRTOS software timer checks that `elapsed_half_periods` advanced
since its last tick whenever the state is RUNNING (worst-case half-period is
13.9 ms @ 36 Hz, so a healthy session advances ~18× per check). On a stall it
forces the gate low and latches a **FAULT** — same latch semantics as the
e-stop: the device prints the session record plus `EVT fault_latched`, and
refuses START until `UNLOCK`.

## Run the safety-core tests (no hardware, no ESP-IDF)

```bash
cd firmware/esp32-gamma-stim
gcc -Wall -Wextra -Werror -O2 -I main tests/test_stim_core.c main/stim_core.c -o /tmp/test_stim && /tmp/test_stim
# -> all 15 stim_core tests passed
```

## Build & flash (ESP-IDF v5.2+)

```bash
idf.py set-target esp32s3        # or esp32c6
idf.py menuconfig                # Gamma Stimulation -> pins, tone freq
idf.py build flash monitor
```

Default pins (Kconfig-overridable): LED GPIO 4, audio GPIO 5, sync-out GPIO 6,
e-stop button GPIO 7 (to GND, active-low), output gate GPIO 10 (to the
driver/amp enables, with external pulldown — see "Output gate wiring").

## Host protocol (line-based, 115200, USB-CDC/UART0)

```
START <freq_mhz> <brightness_pct> <volume_pct> <duration_s>
STOP
STATUS
UNLOCK            # clear a latched e-stop/fault
VERSION
```

`UNLOCK` is refused with `ERR estop_button_pressed` while the physical e-stop
button is still held down — release the button first. (A host retry loop can
therefore never resume a session against a pressed button.) Lines longer than
95 characters are discarded in full (`ERR parse_error`, then everything up to
the next newline is swallowed).

Frequency is **millihertz** (40.0 Hz = `40000`) so the ±0.1 Hz HIL target is
exact integer math (±100 mHz). Example — 40.0 Hz, 30% brightness, 28% volume,
10 min:

```
> START 40000 30 28 600
OK start seq=1 half_period_us=12500
... (session runs) ...
SESSION {"seq":1,"freq_mhz":40000,"brightness_pct":30,"volume_pct":28,"duration_s":600,"half_periods":48000,"stop":"completed","fw":"0.1.0"}
```

The `SESSION {...}` line is canonical (quantized integers, fixed field order) so
the host pairs it with the RuFlo session builder to reproduce the witness hash
(HIL: 100% hash reproducibility).

## How it maps to the HIL targets (`v2/crates/ruview-gamma/src/hil.rs`)

| HIL target | How this firmware meets it |
|------------|----------------------------|
| LED frequency ±0.1 Hz (incl. worst case over the session) | GPTimer at 1 MHz crystal-derived ticks; half-period from exact integer division; worst-case truncation at 44 Hz is ~3 mHz (35× inside budget) |
| A/V sync < 5 ms | one **gate GPIO** enables both the LED driver and the audio amp; on/off skew is zero by construction |
| Stop → actuator off < 100 ms | e-stop GPIO ISR drives the gate GPIO low with a **direct register write** (no driver calls in ISR context) — microseconds; LEDC shutdown follows in task context as defense in depth |
| Stalled-ISR containment | independent 250 ms dead-man timer latches FAULT and forces the gate low if the half-period ISR stops advancing |
| Half-period jitter | gate toggled in a GPTimer alarm ISR with auto-reload; jitter is ISR latency (µs-scale), measured at the sync pin |
| Session-hash reproducibility 100% | canonical integer `SESSION {...}` record, no float formatting, deduped (one record per session seq — STOP-while-idle or e-stop-after-completion never re-print) |
| EEG lift ≥ 20% vs fixed 40 Hz | provided by the host's adaptive optimizer choosing the frequency this firmware plays |

## Hardware notes

- **Drive the LED through a MOSFET/constant-current driver**, not the GPIO
  directly. Keep brightness within eye-safe flicker limits — the firmware caps
  duty at the envelope's 40%, but the optical design owns absolute luminance.
- **Photosensitivity/epilepsy is a hard exclusion** at the host
  (`ExclusionScreen`); the device is the last line, not the only line.
- The e-stop button is mandatory for any human-facing bench run.
