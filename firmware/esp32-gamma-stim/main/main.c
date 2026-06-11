/*
 * esp32-gamma-stim — ESP-IDF hardware binding for the gamma stimulation core.
 *
 * Architecture (ADR-250 §21 M2 device harness, HIL targets in
 * v2/crates/ruview-gamma/src/hil.rs):
 *
 *   GPTimer (1 MHz, crystal-derived) ─ ISR every half-period
 *        ├── GATE:  plain GPIO output-enable, toggled with a direct register
 *        │          write (gpio_ll, IRAM-resident). Carries the 36-44 Hz
 *        │          envelope AND the hard real-time off path.
 *        └── SYNC:  bare GPIO mirroring the envelope (logic-analyzer capture)
 *
 *   LEDC (LED 19.5 kHz carrier, audio tone carrier) runs at a CONSTANT duty
 *   for the whole session, programmed from task context only. The emitted
 *   output is the hardware AND of (LEDC carrier duty) and (GATE asserted):
 *   the gate pin must drive the enable of the LED driver / audio amplifier
 *   (see README "Output gate wiring"). ISRs never call LEDC driver APIs —
 *   they take a non-ISR spinlock and are not IRAM-resident, so calling them
 *   from an ISR is undefined behavior. The stop guarantee rests on a single
 *   GPIO register write instead.
 *
 *   E-STOP button ─ GPIO ISR: gate low FIRST (register write, microseconds),
 *      then latch LOCKED (stim_core) and queue an event; the main task zeroes
 *      the LEDC duty afterwards (defense in depth, not the stop path).
 *
 *   DEAD-MAN ─ independent 250 ms FreeRTOS software timer: while RUNNING the
 *      half-period ISR must advance elapsed_half_periods every <= 13.9 ms
 *      (36 Hz worst case); if a dead-man tick sees no progress the ISR/timer
 *      has died with the envelope possibly ON, so it forces the gate low and
 *      latches a FAULT (same latch semantics as the e-stop).
 *
 *   GPTimer ownership: ONLY the console task starts/stops the GPTimer
 *      (handle_start / STOP). ISRs and the dead-man never call gptimer
 *      functions; after completion/e-stop/fault the timer may idle-tick
 *      harmlessly (state != RUNNING -> gate stays low) until the next host
 *      command reconfigures it. This keeps every driver call in task context.
 *
 *   Host protocol: line-based over USB-CDC/UART0 console at 115200
 *      (START/STOP/STATUS/UNLOCK/VERSION — see stim_core.h). Every session
 *      ends with one "SESSION {...}" JSON line for the host to witness-hash.
 *      UNLOCK is refused while the e-stop button is still held down.
 *
 * All safety decisions (envelope, latch, session math) are in stim_core.c,
 * which is unit-tested on the host. This file only moves registers.
 */
#include <stdio.h>
#include <string.h>

#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "freertos/queue.h"
#include "freertos/timers.h"

#include "driver/gptimer.h"
#include "driver/ledc.h"
#include "driver/gpio.h"
#include "hal/gpio_ll.h"
#include "soc/gpio_struct.h"
#include "esp_log.h"

#include "stim_core.h"

static const char *TAG = "gamma-stim";

#define FIRMWARE_VERSION "0.1.1"

/* ---- Pins / peripherals (Kconfig-overridable) ----------------------------- */
#define PIN_LED      CONFIG_GAMMA_STIM_LED_GPIO
#define PIN_AUDIO    CONFIG_GAMMA_STIM_AUDIO_GPIO
#define PIN_SYNC     CONFIG_GAMMA_STIM_SYNC_GPIO
#define PIN_ESTOP    CONFIG_GAMMA_STIM_ESTOP_GPIO
#define PIN_GATE     CONFIG_GAMMA_STIM_GATE_GPIO

#define LEDC_LED_CH     LEDC_CHANNEL_0
#define LEDC_AUDIO_CH   LEDC_CHANNEL_1
#define LEDC_LED_TIMER  LEDC_TIMER_0
#define LEDC_AUDIO_TIMER LEDC_TIMER_1
/* 12-bit duty at ~19.5 kHz LED carrier: flicker-free dimming far above the
 * envelope band; the 36-44 Hz stimulus is the *envelope*, not the carrier. */
#define LED_CARRIER_HZ   19500
#define LED_DUTY_RES     LEDC_TIMER_12_BIT
#define LED_DUTY_MAX     ((1 << 12) - 1)
/* Audio: square tone carrier gated by the envelope. */
#define AUDIO_TONE_HZ    CONFIG_GAMMA_STIM_AUDIO_TONE_HZ
#define AUDIO_DUTY_RES   LEDC_TIMER_12_BIT
#define AUDIO_DUTY_MAX   ((1 << 12) - 1)

/* Dead-man check period. Worst-case half-period is 13.9 ms (36 Hz), so a
 * healthy session advances elapsed_half_periods ~18+ times per check. */
#define DEADMAN_PERIOD_MS 250

/* ---- Shared state ---------------------------------------------------------- */

static stim_ctx_t s_ctx;                 /* guarded by s_mux: ISRs + tasks   */
static portMUX_TYPE s_mux = portMUX_INITIALIZER_UNLOCKED;
static gptimer_handle_t s_timer = NULL;  /* started/stopped by console task ONLY */
static QueueHandle_t s_evt_queue = NULL; /* events to the main task          */
static TimerHandle_t s_deadman = NULL;
static uint32_t s_printed_seq = 0;       /* guarded by s_mux: record dedupe  */

typedef enum {
    EVT_SESSION_DONE = 1,  /* duration elapsed (RUNNING -> IDLE edge)   */
    EVT_ESTOP = 2,         /* e-stop button latched                     */
    EVT_FAULT = 3,         /* dead-man detected a stalled timer ISR     */
} stim_evt_kind_t;

typedef struct {
    stim_evt_kind_t kind;
    stim_ctx_t snap;       /* context captured at the moment of the event */
} stim_evt_t;

/* ---- ISR-safe GPIO writes (direct register path, inline => IRAM) ---------- */

static inline void IRAM_ATTR gate_write(bool on)
{
    gpio_ll_set_level(&GPIO, PIN_GATE, on ? 1 : 0);
}

static inline void IRAM_ATTR sync_write(bool on)
{
    gpio_ll_set_level(&GPIO, PIN_SYNC, on ? 1 : 0);
}

/* ---- LEDC carrier control (TASK CONTEXT ONLY — not ISR-safe) --------------- */

static void carrier_set(uint8_t brightness_pct, uint8_t volume_pct)
{
    uint32_t led_duty = ((uint32_t)brightness_pct * LED_DUTY_MAX) / 100U;
    /* Volume cap is 40% -> max audio duty 20% of full scale: keep the square
     * tone gentle; real loudness control belongs to the analog stage. */
    uint32_t aud_duty = ((uint32_t)volume_pct * (AUDIO_DUTY_MAX / 2U)) / 100U;
    ESP_ERROR_CHECK(ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_LED_CH, led_duty));
    ESP_ERROR_CHECK(ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_LED_CH));
    ESP_ERROR_CHECK(ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_AUDIO_CH, aud_duty));
    ESP_ERROR_CHECK(ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_AUDIO_CH));
}

static void carrier_off(void)
{
    carrier_set(0, 0);
}

/* ---- ISRs (no driver calls: register writes + queue only) ------------------ */

/* GPTimer alarm ISR: one half-period boundary. The envelope is delivered by
 * toggling the gate GPIO; the LEDC carriers are untouched here. */
static bool IRAM_ATTR on_half_period(gptimer_handle_t timer,
                                     const gptimer_alarm_event_data_t *edata,
                                     void *user)
{
    (void)timer; (void)edata; (void)user;
    BaseType_t hpw = pdFALSE;
    portENTER_CRITICAL_ISR(&s_mux);
    bool was_running = (s_ctx.state == STIM_RUNNING);
    bool running = stim_tick(&s_ctx);
    bool on = running && s_ctx.envelope_on;
    gate_write(on);
    sync_write(on);
    if (was_running && !running) {
        /* RUNNING -> done edge: exactly one completion event per session.
         * The timer is NOT stopped here (gptimer APIs stay in task context);
         * until the next host command it idle-ticks with the gate low. */
        stim_evt_t e = { .kind = EVT_SESSION_DONE, .snap = s_ctx };
        xQueueSendFromISR(s_evt_queue, &e, &hpw);
    }
    portEXIT_CRITICAL_ISR(&s_mux);
    return hpw == pdTRUE;
}

/* E-stop button ISR. Hard real-time stop path = the first line: one direct
 * GPIO register write forcing the output gate low. Everything after that is
 * bookkeeping. LEDC shutdown happens later in the main task. */
static void IRAM_ATTR on_estop(void *arg)
{
    (void)arg;
    gate_write(false);                    /* outputs off: microseconds */
    BaseType_t hpw = pdFALSE;
    portENTER_CRITICAL_ISR(&s_mux);
    bool already_latched = (s_ctx.state == STIM_LOCKED);
    stim_estop(&s_ctx, STIM_STOP_BUTTON);
    sync_write(false);
    if (!already_latched) {               /* debounce: one event per latch */
        stim_evt_t e = { .kind = EVT_ESTOP, .snap = s_ctx };
        xQueueSendFromISR(s_evt_queue, &e, &hpw);
    }
    portEXIT_CRITICAL_ISR(&s_mux);
    if (hpw == pdTRUE) {
        portYIELD_FROM_ISR();
    }
}

/* ---- Dead-man watchdog (FreeRTOS timer task context) ----------------------- */

/* Independent of the GPTimer ISR: if state == RUNNING but the ISR stopped
 * advancing elapsed_half_periods between two checks, the toggling path is
 * dead — possibly with the gate left ON. Force outputs off and latch FAULT
 * (same latch as the e-stop: START refused until UNLOCK). */
static void deadman_cb(TimerHandle_t t)
{
    (void)t;
    static uint32_t dm_seq = 0;       /* session_seq starts at 1; 0 = none */
    static uint32_t dm_elapsed = 0;
    bool fault = false;
    stim_evt_t e = { .kind = EVT_FAULT };
    portENTER_CRITICAL(&s_mux);
    if (s_ctx.state == STIM_RUNNING) {
        if (dm_seq == s_ctx.session_seq &&
            dm_elapsed == s_ctx.elapsed_half_periods) {
            gate_write(false);
            sync_write(false);
            stim_estop(&s_ctx, STIM_STOP_FAULT);
            e.snap = s_ctx;
            fault = true;
        } else {
            dm_seq = s_ctx.session_seq;
            dm_elapsed = s_ctx.elapsed_half_periods;
        }
    } else {
        dm_seq = 0;
        dm_elapsed = 0;
    }
    portEXIT_CRITICAL(&s_mux);
    if (fault) {
        carrier_off();                /* task context: LEDC allowed */
        xQueueSend(s_evt_queue, &e, 0);
    }
}

/* ---- Peripheral setup -------------------------------------------------------- */

static void setup_ledc(void)
{
    ledc_timer_config_t led_t = {
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .timer_num = LEDC_LED_TIMER,
        .duty_resolution = LED_DUTY_RES,
        .freq_hz = LED_CARRIER_HZ,
        .clk_cfg = LEDC_AUTO_CLK,
    };
    ESP_ERROR_CHECK(ledc_timer_config(&led_t));
    ledc_channel_config_t led_c = {
        .gpio_num = PIN_LED,
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .channel = LEDC_LED_CH,
        .timer_sel = LEDC_LED_TIMER,
        .duty = 0,
        .hpoint = 0,
    };
    ESP_ERROR_CHECK(ledc_channel_config(&led_c));

    ledc_timer_config_t aud_t = {
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .timer_num = LEDC_AUDIO_TIMER,
        .duty_resolution = AUDIO_DUTY_RES,
        .freq_hz = AUDIO_TONE_HZ,
        .clk_cfg = LEDC_AUTO_CLK,
    };
    ESP_ERROR_CHECK(ledc_timer_config(&aud_t));
    ledc_channel_config_t aud_c = {
        .gpio_num = PIN_AUDIO,
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .channel = LEDC_AUDIO_CH,
        .timer_sel = LEDC_AUDIO_TIMER,
        .duty = 0,
        .hpoint = 0,
    };
    ESP_ERROR_CHECK(ledc_channel_config(&aud_c));
}

static void setup_gpio(void)
{
    /* Output-enable gate FIRST: drive it low before the LEDC carriers exist.
     * The board-level wiring must add an external pulldown so the LED driver
     * and audio amp stay disabled while this pin floats during boot/reset
     * (see README "Output gate wiring"). */
    gpio_config_t gate = {
        .pin_bit_mask = 1ULL << PIN_GATE,
        .mode = GPIO_MODE_OUTPUT,
        .pull_down_en = GPIO_PULLDOWN_ENABLE,
    };
    ESP_ERROR_CHECK(gpio_config(&gate));
    gpio_set_level(PIN_GATE, 0);

    gpio_config_t sync = {
        .pin_bit_mask = 1ULL << PIN_SYNC,
        .mode = GPIO_MODE_OUTPUT,
    };
    ESP_ERROR_CHECK(gpio_config(&sync));
    gpio_set_level(PIN_SYNC, 0);

    gpio_config_t estop = {
        .pin_bit_mask = 1ULL << PIN_ESTOP,
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,    /* button to GND, active low */
        .intr_type = GPIO_INTR_NEGEDGE,
    };
    ESP_ERROR_CHECK(gpio_config(&estop));
    ESP_ERROR_CHECK(gpio_install_isr_service(0));
    ESP_ERROR_CHECK(gpio_isr_handler_add(PIN_ESTOP, on_estop, NULL));
}

static void setup_timer(void)
{
    gptimer_config_t cfg = {
        .clk_src = GPTIMER_CLK_SRC_DEFAULT,
        .direction = GPTIMER_COUNT_UP,
        .resolution_hz = 1000000, /* 1 us ticks, crystal-derived */
    };
    ESP_ERROR_CHECK(gptimer_new_timer(&cfg, &s_timer));
    gptimer_event_callbacks_t cbs = { .on_alarm = on_half_period };
    ESP_ERROR_CHECK(gptimer_register_event_callbacks(s_timer, &cbs, NULL));
    ESP_ERROR_CHECK(gptimer_enable(s_timer));
}

/* ---- Session lifecycle ---------------------------------------------------------- */

/* Print the canonical witness record for a finished session. The snapshot is
 * captured at the event source (under s_mux), so a new session racing in
 * cannot corrupt the record. Deduped by session_seq: STOP-while-idle or an
 * e-stop pressed after completion must NOT re-print the previous record. */
static void print_session_record(const stim_ctx_t *snap)
{
    if (snap->session_seq == 0 || snap->state == STIM_RUNNING) {
        return; /* no finished session to report */
    }
    portENTER_CRITICAL(&s_mux);
    bool dup = (snap->session_seq == s_printed_seq);
    if (!dup) {
        s_printed_seq = snap->session_seq;
    }
    portEXIT_CRITICAL(&s_mux);
    if (dup) {
        return;
    }
    /* One canonical JSON line per finished session; the host pairs it with the
     * RuFlo session builder to compute the witness hash (HIL: 100% hash
     * reproducibility). Quantized integers only — no float formatting drift. */
    printf("SESSION {\"seq\":%u,\"freq_mhz\":%u,\"brightness_pct\":%u,"
           "\"volume_pct\":%u,\"duration_s\":%u,\"half_periods\":%u,"
           "\"stop\":\"%s\",\"fw\":\"%s\"}\n",
           (unsigned)snap->session_seq, (unsigned)snap->active.freq_mhz,
           (unsigned)snap->active.brightness_pct, (unsigned)snap->active.volume_pct,
           (unsigned)snap->active.duration_s, (unsigned)snap->elapsed_half_periods,
           stim_stop_str(snap->last_stop), FIRMWARE_VERSION);
}

static void handle_start(const stim_params_t *p)
{
    portENTER_CRITICAL(&s_mux);
    stim_rc_t rc = stim_start(&s_ctx, p);
    uint32_t seq = s_ctx.session_seq;
    portEXIT_CRITICAL(&s_mux);
    if (rc != STIM_OK) {
        printf("ERR %s\n", stim_rc_str(rc));
        return;
    }

    /* Program the carriers at session intensity (task context). Nothing is
     * emitted yet: the gate GPIO stays low until the first ON half-period. */
    carrier_set(p->brightness_pct, p->volume_pct);

    uint32_t half_us = stim_half_period_us(p->freq_mhz);
    gptimer_alarm_config_t alarm = {
        .alarm_count = half_us,
        .reload_count = 0,
        .flags.auto_reload_on_alarm = true,
    };
    gptimer_stop(s_timer); /* idempotent: may be idle-ticking or stopped */
    ESP_ERROR_CHECK(gptimer_set_raw_count(s_timer, 0));
    ESP_ERROR_CHECK(gptimer_set_alarm_action(s_timer, &alarm));

    /* TOCTOU guard: an e-stop may have latched between the RUNNING commit
     * above and here. Start the timer under the same mux the e-stop ISR
     * takes, so "state == LOCKED" and "timer started for this session" can
     * never coexist. (gptimer_start error, if any, is checked outside the
     * critical section because it may log.) */
    esp_err_t start_err = ESP_OK;
    portENTER_CRITICAL(&s_mux);
    bool still_running =
        (s_ctx.state == STIM_RUNNING && s_ctx.session_seq == seq);
    if (still_running) {
        start_err = gptimer_start(s_timer);
    }
    portEXIT_CRITICAL(&s_mux);
    if (!still_running) {
        carrier_off(); /* gate already low (forced by the e-stop ISR) */
        printf("ERR %s\n", stim_rc_str(STIM_ERR_LOCKED));
        return;
    }
    ESP_ERROR_CHECK(start_err);
    printf("OK start seq=%u half_period_us=%u\n",
           (unsigned)seq, (unsigned)half_us);
}

static void handle_line(const char *line)
{
    stim_cmd_t cmd;
    stim_rc_t rc = stim_parse_line(line, &cmd);
    if (rc != STIM_OK) {
        printf("ERR %s\n", stim_rc_str(rc));
        return;
    }
    switch (cmd.kind) {
    case STIM_CMD_START:
        handle_start(&cmd.params);
        break;
    case STIM_CMD_STOP: {
        portENTER_CRITICAL(&s_mux);
        gate_write(false);
        sync_write(false);
        stim_stop_host(&s_ctx);
        stim_ctx_t snap = s_ctx;
        portEXIT_CRITICAL(&s_mux);
        gptimer_stop(s_timer); /* console task owns the timer; rc ignored */
        carrier_off();
        print_session_record(&snap); /* deduped: no-op if nothing new ran */
        printf("OK stop\n");
        break;
    }
    case STIM_CMD_STATUS: {
        portENTER_CRITICAL(&s_mux);
        stim_ctx_t snap = s_ctx;
        portEXIT_CRITICAL(&s_mux);
        const char *st = snap.state == STIM_RUNNING ? "running"
                       : snap.state == STIM_LOCKED  ? "locked"
                                                    : "idle";
        printf("OK status state=%s seq=%u last_stop=%s\n", st,
               (unsigned)snap.session_seq, stim_stop_str(snap.last_stop));
        break;
    }
    case STIM_CMD_UNLOCK:
        /* Refuse to clear the latch while the physical e-stop button is still
         * held (active low): a host retry loop must never be able to resume
         * against a pressed button. If the button is pressed again right
         * after this check, the NEGEDGE ISR simply re-latches. */
        if (gpio_get_level(PIN_ESTOP) == 0) {
            printf("ERR estop_button_pressed\n");
            break;
        }
        portENTER_CRITICAL(&s_mux);
        stim_unlock(&s_ctx);
        portEXIT_CRITICAL(&s_mux);
        printf("OK unlock\n");
        break;
    case STIM_CMD_VERSION:
        printf("OK version fw=%s envelope=36000-44000mHz b<=%u%% v<=%u%% d<=%us\n",
               FIRMWARE_VERSION,
               (unsigned)s_ctx.envelope.max_brightness_pct,
               (unsigned)s_ctx.envelope.max_volume_pct,
               (unsigned)s_ctx.envelope.max_duration_s);
        break;
    default:
        printf("ERR %s\n", stim_rc_str(STIM_ERR_UNKNOWN_CMD));
    }
}

/* Console reader: line-buffered stdin (USB-CDC / UART0). */
static void console_task(void *arg)
{
    (void)arg;
    char buf[96];
    size_t n = 0;
    bool discard = false; /* swallowing the tail of an overlong line */
    for (;;) {
        int ch = fgetc(stdin);
        if (ch == EOF) {
            vTaskDelay(pdMS_TO_TICKS(10));
            continue;
        }
        if (ch == '\r') {
            continue;
        }
        if (ch == '\n') {
            if (discard) {
                discard = false; /* overlong line fully consumed */
                n = 0;
                continue;
            }
            buf[n] = '\0';
            if (n > 0) {
                handle_line(buf);
            }
            n = 0;
            continue;
        }
        if (discard) {
            continue;
        }
        if (n + 1 < sizeof(buf)) {
            buf[n++] = (char)ch;
        } else {
            /* Overlong line: fail closed — drop it AND everything up to the
             * next newline, so the tail is never reinterpreted as a fresh
             * command. */
            discard = true;
            n = 0;
            printf("ERR %s\n", stim_rc_str(STIM_ERR_PARSE));
        }
    }
}

void app_main(void)
{
    ESP_LOGI(TAG, "gamma-stim v%s (ADR-250 M2 device harness)", FIRMWARE_VERSION);
    s_evt_queue = xQueueCreate(8, sizeof(stim_evt_t));
    configASSERT(s_evt_queue != NULL);
    stim_init(&s_ctx, stim_envelope_conservative());
    setup_gpio();   /* gate low before the LEDC carriers are configured */
    setup_ledc();
    setup_timer();
    carrier_off();

    s_deadman = xTimerCreate("deadman", pdMS_TO_TICKS(DEADMAN_PERIOD_MS),
                             pdTRUE, NULL, deadman_cb);
    configASSERT(s_deadman != NULL);
    xTimerStart(s_deadman, 0);

    xTaskCreate(console_task, "console", 4096, NULL, 5, NULL);
    ESP_LOGI(TAG, "ready: envelope 36.0-44.0 Hz, brightness<=%u%%, volume<=%u%%",
             (unsigned)s_ctx.envelope.max_brightness_pct,
             (unsigned)s_ctx.envelope.max_volume_pct);

    stim_evt_t evt;
    for (;;) {
        if (xQueueReceive(s_evt_queue, &evt, portMAX_DELAY) == pdTRUE) {
            /* The hard off (gate low) already happened at the event source.
             * Zero the carriers here in task context — but never behind a
             * session that has legitimately started since the event fired. */
            bool cleanup;
            portENTER_CRITICAL(&s_mux);
            cleanup = (s_ctx.state != STIM_RUNNING);
            portEXIT_CRITICAL(&s_mux);
            if (cleanup) {
                carrier_off();
            }
            switch (evt.kind) {
            case EVT_SESSION_DONE:
                print_session_record(&evt.snap);
                break;
            case EVT_ESTOP:
                print_session_record(&evt.snap);
                printf("EVT estop_latched\n");
                break;
            case EVT_FAULT:
                print_session_record(&evt.snap);
                printf("EVT fault_latched\n");
                break;
            }
        }
    }
}
