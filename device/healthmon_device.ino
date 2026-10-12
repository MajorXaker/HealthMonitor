/**
 * healthmon_device.ino — v1.2
 *
 * Hardware status indicator + OLED UI for the healthmon backend.
 * Target board: Seeed Studio XIAO ESP32-C6
 *
 * Features
 * --------
 * - Connects to Wi-Fi and periodically polls the healthmon REST API.
 * - Drives 4 LEDs to reflect system state:
 *     LED_OK       (green/blue) – solid when backend reachable + all checks OK
 *     LED_ISSUES   (red)        – pulsing when any healthcheck fails
 *     LED_EMAIL_OK (green/blue) – solid when email subsystem is online
 *     LED_EMAILS   (yellow)     – pulsing when new (unread) emails are present
 * - 128x64 monochrome I2C OLED (SSD1306 or SH1106) driven by U8g2,
 *   UTF-8 text with Cyrillic glyphs.
 * - Rotary encoder (CLK/DT) switches screens: clockwise = next,
 *   counter-clockwise = previous; the list wraps around.
 *   The encoder push switch (SW) is wired but intentionally unused.
 * - Screens, in order:
 *     1. One screen per new email (sender, subject, date, account)
 *     2. Healthcheck pages (one line per service, 4 per page)
 *     3. Currency rates (plain text; placeholder until backend support lands)
 * - Acknowledge button:
 *     Short press on an email screen → acknowledge that one email and show
 *       the next screen. Short press elsewhere → nothing.
 *     Hold for 5 s → acknowledge ALL emails. While holding, a progress bar
 *       fills the first pixel row of the display.
 * - All network I/O runs in a separate FreeRTOS task, so the display,
 *   encoder and button stay responsive while HTTP requests are in flight.
 * - Display blanks after inactivity (configurable, 0 = never) to limit OLED
 *   burn-in. The first input after blanking only wakes the display; a newly
 *   arrived email wakes it as well.
 * - PWM pulsing via ESP32 LEDC hardware peripheral.
 *
 * Wiring (see WIRING.md)
 * ----------------------
 * D0  → LED_OK       anode (via 220 Ω resistor to GND)
 * D1  → LED_ISSUES   anode (via 220 Ω resistor to GND)
 * D2  → LED_EMAIL_OK anode (via 220 Ω resistor to GND)
 * D3  → LED_EMAILS   anode (via 220 Ω resistor to GND)
 * D4  → Display SDA
 * D5  → Display SCL
 * D7  → Encoder SW   (unused for now)
 * D8  → Encoder CLK
 * D9  → Encoder DT
 * D10 → Acknowledge button (other leg to GND, internal pull-up enabled)
 * 3V3 → Display VCC, encoder "+"   (NOT 5V — the module pull-ups use this rail)
 *
 * Arduino IDE setup
 * -----------------
 * 1. File → Preferences → Additional boards manager URLs:
 *    https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json
 * 2. Tools → Board → Boards Manager → install "esp32 by Espressif Systems" (≥3.x)
 * 3. Tools → Board → ESP32 Arduino → "XIAO_ESP32C6"
 * 4. Tools → USB CDC On Boot → "Enabled" (serial log over USB-C)
 * 5. Tools → Port → select the USB-CDC port, then Sketch → Upload
 *
 * Libraries required:
 *   WiFi.h, HTTPClient.h, base64.h — bundled with ESP32 Arduino core
 *   ArduinoJson — Library Manager (Benoit Blanchon, ≥7.x)
 *   U8g2        — Library Manager (oliver, ≥2.34)
 */

// ─── Dependencies ────────────────────────────────────────────────────────────

#include <WiFi.h>
#include <HTTPClient.h>
#include <ArduinoJson.h>
#include <base64.h>  // ESP32 Arduino core built-in
#include <U8g2lib.h>
#include <Wire.h>
#include <time.h>

// ═══════════════════════════════════════════════════════════════════════════
//  CONFIGURATION — edit these values before flashing.
//  All settings are baked into the firmware; a config change requires reflash.
// ═══════════════════════════════════════════════════════════════════════════

namespace Config {
  // Wi-Fi credentials
  constexpr const char* WIFI_SSID     = "WIFI_SSID";
  constexpr const char* WIFI_PASSWORD = "WIFI_PASSWORD";

  // healthmon backend
  constexpr const char* BACKEND_HOST  = "http://localhost:8000";
  constexpr const char* BASIC_AUTH    = "admin:password";  // user:password

  // Plain-text currency endpoint (path on BACKEND_HOST), backend ≥ v1.2.
  // The body (text/plain, one rate per line, 21 chars) is shown as is,
  // 4 lines per screen. nullptr = disabled → placeholder screen.
  // Against an older backend the request returns 404 and the placeholder stays.
  constexpr const char* CURRENCY_PATH = "/currency";

  // Polling interval for healthcheck + email status (milliseconds)
  constexpr unsigned long POLL_INTERVAL_MS = 30000;

  // HTTP request timeout (milliseconds)
  constexpr int HTTP_TIMEOUT_MS = 8000;

  // Wi-Fi: how long to wait for a connection before restarting the attempt
  constexpr unsigned long WIFI_RETRY_MS = 20000;

  // Button debounce window (milliseconds)
  constexpr unsigned long DEBOUNCE_MS = 50;

  // Hold time for "acknowledge ALL emails" (milliseconds)
  constexpr unsigned long ACK_ALL_HOLD_MS = 5000;

  // Rotary encoder: quadrature transitions per mechanical click.
  // Most clicky KY-040-style encoders produce 4; some produce 2.
  // If one click needs two turns to change the screen, set 2.
  constexpr int8_t ENCODER_STEPS_PER_DETENT = 4;
  // Set true if clockwise rotation goes to the previous screen.
  constexpr bool   ENCODER_REVERSE = false;

  // Display
  constexpr unsigned long DISPLAY_TIMEOUT_MS = 300000; // blank after 5 min idle; 0 = never
  constexpr uint32_t      RENDER_INTERVAL_MS = 40;     // ~25 fps
  constexpr uint8_t       DISPLAY_CONTRAST   = 128;    // 0–255

  // Local time offset for email dates shown on screen (minutes from UTC).
  // Europe/Minsk = +180.
  constexpr int TZ_OFFSET_MINUTES = 180;

  // PWM pulsing: period for one full bright→dim→bright cycle (milliseconds)
  constexpr unsigned long PULSE_PERIOD_MS = 1500;

  // LEDC PWM settings
  constexpr uint32_t PWM_FREQ  = 5000;  // Hz
  constexpr uint8_t  PWM_BITS  = 8;     // resolution: 0–255
  constexpr uint8_t  PWM_MAX   = 255;
  constexpr uint8_t  PWM_MIN   = 5;     // floor during pulsing (never fully off)

  // Steady-on brightness (0–255) for LED_EMAIL_OK.
  // 255 = full brightness, 60 ≈ 25%, 30 ≈ 12%.
  constexpr uint8_t  PWM_STEADY = 15;

  // Acknowledge blink animation: quick flashes on LED_EMAILS after acknowledge.
  constexpr uint8_t  ACK_BLINK_COUNT    = 3;
  constexpr uint32_t ACK_BLINK_ON_MS    = 80;   // LED on duration per blink
  constexpr uint32_t ACK_BLINK_OFF_MS   = 100;  // LED off gap between blinks
}

// ─── Limits (RAM budget) ─────────────────────────────────────────────────────

namespace Limits {
  constexpr uint8_t MAX_EMAILS        = 10;  // emails kept on the device
  constexpr uint8_t MAX_CHECKS        = 16;  // healthchecks kept on the device
  constexpr uint8_t CHECKS_PER_PAGE   = 4;   // lines per healthcheck screen
  constexpr uint8_t MAX_HIDDEN        = 16;  // locally acknowledged, awaiting backend
  constexpr size_t  SENDER_LEN        = 64;  // bytes, UTF-8 (Cyrillic = 2 bytes/char)
  constexpr size_t  SUBJECT_LEN       = 112;
  constexpr size_t  ACCOUNT_LEN       = 32;
  constexpr size_t  CHECK_NAME_LEN    = 48;
  constexpr size_t  CURRENCY_LEN      = 384;
  constexpr uint8_t LINES_PER_PAGE    = 4;   // content lines per screen
  constexpr uint8_t MAX_RATE_PAGES    = 4;
  constexpr uint8_t MAX_SCREENS       =
      MAX_EMAILS + (MAX_CHECKS + CHECKS_PER_PAGE - 1) / CHECKS_PER_PAGE + MAX_RATE_PAGES;
}

// ─── Pin assignments ──────────────────────────────────────────────────────────

namespace Pins {
  constexpr uint8_t LED_OK       = D0;   // "all is ok" (green/blue)
  constexpr uint8_t LED_ISSUES   = D1;   // "healthcheck issues" (red)
  constexpr uint8_t LED_EMAIL_OK = D2;   // "email subsystem online" (green/blue)
  constexpr uint8_t LED_EMAILS   = D3;   // "new emails" (yellow)
  constexpr uint8_t DISPLAY_SDA  = D4;   // GPIO22, default I2C SDA
  constexpr uint8_t DISPLAY_SCL  = D5;   // GPIO23, default I2C SCL
  // D6 (GPIO16, UART0 TX) is left free: the ROM may print a boot log on it.
  constexpr uint8_t ENC_SW       = D7;   // GPIO17 — encoder push, unused for now
  constexpr uint8_t ENC_CLK      = D8;   // GPIO19
  constexpr uint8_t ENC_DT       = D9;   // GPIO20
  constexpr uint8_t BUTTON       = D10;  // GPIO18 — acknowledge button
}

// ─── Display driver ──────────────────────────────────────────────────────────
// Pick ONE constructor matching your module (both: hardware I2C, full buffer).
//   0.96" modules are usually SSD1306, 1.3" modules are usually SH1106.
// If the picture is shifted by 2 px with garbage on one edge, you have an
// SH1106 — switch the constructor.

U8G2_SSD1306_128X64_NONAME_F_HW_I2C u8g2(U8G2_R0, /* reset=*/ U8X8_PIN_NONE);
// U8G2_SH1106_128X64_NONAME_F_HW_I2C u8g2(U8G2_R0, /* reset=*/ U8X8_PIN_NONE);

// Fonts: 6x12 monospace with Latin + Cyrillic → 21 characters per line.
#define FONT_MAIN u8g2_font_6x12_t_cyrillic

// ═══════════════════════════════════════════════════════════════════════════
//  Data model
// ═══════════════════════════════════════════════════════════════════════════

struct EmailRec {
  int32_t id = -1;                           ///< DB id, used by acknowledge
  char    sender[Limits::SENDER_LEN]   = {0};
  char    subject[Limits::SUBJECT_LEN] = {0};
  char    account[Limits::ACCOUNT_LEN] = {0};
  char    date[12]                     = {0}; ///< "MM-DD HH:MM", local time
};

struct CheckRec {
  char name[Limits::CHECK_NAME_LEN] = {0};
  bool healthy = false;
};

/**
 * Everything the network task learns from the backend.
 * Produced by the network task, copied under a mutex into the UI.
 */
struct Snapshot {
  bool     wifiUp           = false;
  bool     backendReachable = false;
  bool     polled           = false;   ///< at least one poll attempt finished
  bool     hasData          = false;   ///< at least one successful poll happened
  uint32_t lastOkMs         = 0;       ///< millis() of last successful poll
  uint32_t ackSeq           = 0;       ///< ack commands processed before this poll

  bool     allChecksHealthy = false;
  bool     emailSubsystemOn = false;

  EmailRec emails[Limits::MAX_EMAILS];
  uint8_t  emailCount       = 0;       ///< stored on device
  uint16_t emailTotal       = 0;       ///< reported by backend

  CheckRec checks[Limits::MAX_CHECKS];
  uint8_t  checkCount       = 0;
  uint16_t checkTotal       = 0;

  char     currency[Limits::CURRENCY_LEN] = {0};
};

/** Command sent from the UI to the network task. */
struct AckCommand {
  enum class Kind : uint8_t { SINGLE, ALL } kind;
  int32_t  id;    ///< email id for SINGLE
  uint32_t seq;   ///< monotonically increasing command number
};

enum class ScreenType : uint8_t { EMAIL, HEALTH, CURRENCY };

struct ScreenRef {
  ScreenType type;
  int32_t    key;   ///< email id, healthcheck page index, or 0
};

// ═══════════════════════════════════════════════════════════════════════════
//  UTF-8 helpers (U8g2 draws UTF-8; truncation must not split a character)
// ═══════════════════════════════════════════════════════════════════════════

/** Number of bytes covering the first `maxChars` UTF-8 characters of `s`. */
size_t utf8PrefixBytes(const char* s, size_t maxChars) {
  size_t bytes = 0, chars = 0;
  while (s[bytes] && chars < maxChars) {
    bytes++;
    while ((s[bytes] & 0xC0) == 0x80) bytes++;  // skip continuation bytes
    chars++;
  }
  return bytes;
}

size_t utf8Length(const char* s) {
  size_t n = 0;
  for (; *s; s++) if ((*s & 0xC0) != 0x80) n++;
  return n;
}

/** Copy `src` into `dst` (capacity `cap`), never splitting a UTF-8 sequence. */
void copyUtf8(char* dst, size_t cap, const char* src) {
  if (!src) { dst[0] = 0; return; }
  size_t n = strlen(src);
  if (n >= cap) {
    n = cap - 1;
    while (n > 0 && (src[n] & 0xC0) == 0x80) n--;  // back off to a char start
  }
  memcpy(dst, src, n);
  dst[n] = 0;
  // Replace control characters (tabs, CR/LF in folded headers) with spaces.
  for (size_t i = 0; i < n; i++) if ((uint8_t)dst[i] < 0x20) dst[i] = ' ';
}

/**
 * Write at most `maxChars` characters of `s` into `out`; if `s` is longer,
 * end it with ".." so the cut is visible.
 */
void fitLine(char* out, size_t cap, const char* s, size_t maxChars) {
  if (maxChars < 3) { out[0] = 0; return; }
  if (utf8Length(s) <= maxChars) {
    copyUtf8(out, cap, s);
    return;
  }
  size_t b = utf8PrefixBytes(s, maxChars - 2);
  if (b > cap - 3) b = cap - 3;
  memcpy(out, s, b);
  out[b] = '.'; out[b + 1] = '.'; out[b + 2] = 0;
}

// ─── Date helpers ────────────────────────────────────────────────────────────

/** Days since 1970-01-01 for a civil date (H. Hinnant's algorithm). */
int32_t daysFromCivil(int32_t y, uint32_t m, uint32_t d) {
  y -= m <= 2;
  const int32_t  era = (y >= 0 ? y : y - 399) / 400;
  const uint32_t yoe = (uint32_t)(y - era * 400);
  const uint32_t doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  const uint32_t doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  return era * 146097 + (int32_t)doe - 719468;
}

/** "2026-10-12T08:15:30.123Z" (UTC) → "10-12 11:15" in configured local time. */
void formatEmailDate(const char* iso, char* out, size_t cap) {
  int y, mo, d, h, mi;
  if (!iso || sscanf(iso, "%4d-%2d-%2dT%2d:%2d", &y, &mo, &d, &h, &mi) != 5) {
    copyUtf8(out, cap, "");
    return;
  }
  time_t t = (time_t)daysFromCivil(y, mo, d) * 86400 + h * 3600 + mi * 60
           + (time_t)Config::TZ_OFFSET_MINUTES * 60;
  struct tm tmv;
  gmtime_r(&t, &tmv);
  snprintf(out, cap, "%02d-%02d %02d:%02d",
           tmv.tm_mon + 1, tmv.tm_mday, tmv.tm_hour, tmv.tm_min);
}

// ═══════════════════════════════════════════════════════════════════════════
//  AckBlinker
//  Non-blocking flash animation played on LED_EMAILS after an acknowledge.
// ═══════════════════════════════════════════════════════════════════════════

class AckBlinker {
public:
  using DoneCallback = void (*)();

  /** Start the blink sequence. Safe to call even if already running (restarts). */
  void start(DoneCallback onDone) {
    _onDone     = onDone;
    _blinks     = 0;
    _phase      = Phase::ON;
    _phaseStart = millis();
    _running    = true;
    ledcWrite(Pins::LED_EMAILS, Config::PWM_MAX);  // first blink on immediately
  }

  bool running() const { return _running; }

  /** Advance the state machine. Must be called every loop() iteration. */
  void update() {
    if (!_running) return;
    uint32_t now     = millis();
    uint32_t elapsed = now - _phaseStart;

    if (_phase == Phase::ON && elapsed >= Config::ACK_BLINK_ON_MS) {
      ledcWrite(Pins::LED_EMAILS, 0);
      _blinks++;
      if (_blinks >= Config::ACK_BLINK_COUNT) {
        _running = false;
        if (_onDone) _onDone();
      } else {
        _phase      = Phase::OFF;
        _phaseStart = now;
      }
    } else if (_phase == Phase::OFF && elapsed >= Config::ACK_BLINK_OFF_MS) {
      ledcWrite(Pins::LED_EMAILS, Config::PWM_MAX);
      _phase      = Phase::ON;
      _phaseStart = now;
    }
  }

private:
  enum class Phase { ON, OFF };
  bool         _running    = false;
  Phase        _phase      = Phase::ON;
  uint32_t     _phaseStart = 0;
  uint8_t      _blinks     = 0;
  DoneCallback _onDone     = nullptr;
};

// ═══════════════════════════════════════════════════════════════════════════
//  LedController
//  Four LEDs, each OFF / ON / PULSING (sinusoidal PWM).
// ═══════════════════════════════════════════════════════════════════════════

class LedController {
public:
  enum class Mode { OFF, ON, PULSING };

  void begin() {
    ledcAttach(Pins::LED_OK,       Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_ISSUES,   Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAIL_OK, Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAILS,   Config::PWM_FREQ, Config::PWM_BITS);
  }

  void setMode(uint8_t pin, Mode mode) {
    getSlot(pin).mode = mode;
    if (mode == Mode::OFF) ledcWrite(pin, 0);
    if (mode == Mode::ON) {
      uint32_t brightness = (pin == Pins::LED_EMAIL_OK) ? Config::PWM_STEADY : 255;
      ledcWrite(pin, brightness);
    }
    // PULSING duty is updated each tick in update()
  }

  /** Update pulsing LEDs. Skips LED_EMAILS while the AckBlinker owns it. */
  void update(bool skipEmailsPin = false) {
    uint32_t phase = millis() % Config::PULSE_PERIOD_MS;
    float    t     = (float)phase / (float)Config::PULSE_PERIOD_MS;
    float    sine  = (sinf(t * 2.0f * PI) + 1.0f) / 2.0f;  // [0, 1]
    uint8_t  duty  = (uint8_t)(Config::PWM_MIN + sine * (Config::PWM_MAX - Config::PWM_MIN));

    for (auto& slot : _slots) {
      if (slot.mode != Mode::PULSING) continue;
      if (skipEmailsPin && slot.pin == Pins::LED_EMAILS) continue;
      ledcWrite(slot.pin, duty);
    }
  }

private:
  struct Slot { uint8_t pin; Mode mode = Mode::OFF; };
  Slot _slots[4] = {
    { Pins::LED_OK }, { Pins::LED_ISSUES }, { Pins::LED_EMAIL_OK }, { Pins::LED_EMAILS },
  };
  Slot& getSlot(uint8_t pin) {
    for (auto& s : _slots) if (s.pin == pin) return s;
    return _slots[0];
  }
};

// ═══════════════════════════════════════════════════════════════════════════
//  Encoder
//  Interrupt-driven quadrature decoder (state-transition table). Invalid
//  transitions caused by contact bounce are ignored, and steps are counted
//  even if the main loop is briefly busy.
// ═══════════════════════════════════════════════════════════════════════════

namespace Encoder {
  volatile int32_t  rawSteps  = 0;
  volatile uint8_t  lastState = 0;
  portMUX_TYPE      mux       = portMUX_INITIALIZER_UNLOCKED;

  // Index = (previous AB << 2) | current AB → -1, 0 or +1
  const int8_t TABLE[16] = { 0, -1,  1,  0,
                             1,  0,  0, -1,
                            -1,  0,  0,  1,
                             0,  1, -1,  0 };

  inline uint8_t readAB() {
    return (digitalRead(Pins::ENC_CLK) << 1) | digitalRead(Pins::ENC_DT);
  }

  void IRAM_ATTR isr() {
    uint8_t s = readAB();
    portENTER_CRITICAL_ISR(&mux);
    rawSteps += TABLE[(lastState << 2) | s];
    lastState = s;
    portEXIT_CRITICAL_ISR(&mux);
  }

  void begin() {
    pinMode(Pins::ENC_CLK, INPUT_PULLUP);
    pinMode(Pins::ENC_DT,  INPUT_PULLUP);
    pinMode(Pins::ENC_SW,  INPUT_PULLUP);  // wired, unused for now
    lastState = readAB();
    attachInterrupt(digitalPinToInterrupt(Pins::ENC_CLK), isr, CHANGE);
    attachInterrupt(digitalPinToInterrupt(Pins::ENC_DT),  isr, CHANGE);
  }

  /** Returns whole clicks since last call (+ = clockwise); keeps the remainder. */
  int32_t takeDetents() {
    portENTER_CRITICAL(&mux);
    int32_t detents = rawSteps / Config::ENCODER_STEPS_PER_DETENT;
    rawSteps -= detents * Config::ENCODER_STEPS_PER_DETENT;
    portEXIT_CRITICAL(&mux);
    return Config::ENCODER_REVERSE ? -detents : detents;
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  AckButton
//  Debounced button with two gestures: short press (acted on release) and
//  long hold (fires once when the hold time is reached, release ignored).
// ═══════════════════════════════════════════════════════════════════════════

class AckButton {
public:
  struct Events {
    bool pressed    = false;  ///< debounced press started
    bool shortPress = false;  ///< released before the hold threshold
    bool holdDone   = false;  ///< hold threshold reached (fires once per press)
  };

  void begin(uint8_t pin) {
    _pin = pin;
    pinMode(pin, INPUT_PULLUP);
  }

  /** Poll once per loop() iteration. */
  Events update() {
    Events ev;
    bool reading = (digitalRead(_pin) == LOW);
    uint32_t now = millis();

    if (reading != _lastReading) {
      _debounceStart = now;
      _lastReading   = reading;
    }
    if ((now - _debounceStart) < Config::DEBOUNCE_MS) return ev;

    if (reading && !_held) {               // press
      _held       = true;
      _holdFired  = false;
      _pressStart = now;
      ev.pressed  = true;
    } else if (!reading && _held) {        // release
      _held = false;
      if (!_holdFired) ev.shortPress = true;
    } else if (reading && !_holdFired && (now - _pressStart) >= Config::ACK_ALL_HOLD_MS) {
      _holdFired  = true;
      ev.holdDone = true;
    }
    return ev;
  }

  bool     isHeld()    const { return _held; }
  bool     holdFired() const { return _holdFired; }
  uint32_t heldFor()   const { return _held ? millis() - _pressStart : 0; }

private:
  uint8_t  _pin           = 0;
  bool     _lastReading   = false;
  bool     _held          = false;
  bool     _holdFired     = false;
  uint32_t _debounceStart = 0;
  uint32_t _pressStart    = 0;
};

// ═══════════════════════════════════════════════════════════════════════════
//  Shared state between the UI loop and the network task
// ═══════════════════════════════════════════════════════════════════════════

SemaphoreHandle_t gSnapMutex = nullptr;  ///< guards gShared + gSharedVersion
QueueHandle_t     gAckQueue  = nullptr;  ///< UI → network task
Snapshot          gShared;               ///< last snapshot published by network task
volatile uint32_t gSharedVersion = 0;

// ═══════════════════════════════════════════════════════════════════════════
//  Network task — the only code that touches Wi-Fi / HTTP
// ═══════════════════════════════════════════════════════════════════════════

namespace Net {
  Snapshot work;   // network task's private copy (static: too big for the stack)

  String authHeader() {
    return "Basic " + base64::encode(String(Config::BASIC_AUTH));
  }

  /** GET `path`; on HTTP 200 store body in `body` and return true. */
  bool get(const char* path, String& body) {
    HTTPClient http;
    http.begin(String(Config::BACKEND_HOST) + path);
    http.addHeader("Authorization", authHeader());
    http.setTimeout(Config::HTTP_TIMEOUT_MS);
    int code = http.GET();
    if (code == 200) body = http.getString();
    else Serial.printf("[net] GET %s → HTTP %d\n", path, code);
    http.end();
    return code == 200;
  }

  bool post(const char* path, const String& payload) {
    HTTPClient http;
    http.begin(String(Config::BACKEND_HOST) + path);
    http.addHeader("Authorization", authHeader());
    http.addHeader("Content-Type", "application/json");
    http.setTimeout(Config::HTTP_TIMEOUT_MS);
    int code = http.POST(payload);
    Serial.printf("[net] POST %s %s → HTTP %d\n", path, payload.c_str(), code);
    http.end();
    return code == 200;
  }

  /** GET /healthchecks → work.checks / allChecksHealthy. */
  bool fetchHealthchecks() {
    String body;
    if (!get("/healthchecks", body)) { work.allChecksHealthy = false; return false; }

    JsonDocument filter;
    filter[0]["name"]    = true;
    filter[0]["healthy"] = true;
    JsonDocument doc;
    DeserializationError err =
        deserializeJson(doc, body, DeserializationOption::Filter(filter));
    if (err) {
      Serial.printf("[net] healthchecks JSON error: %s\n", err.c_str());
      work.allChecksHealthy = false;
      return false;
    }

    JsonArray arr = doc.as<JsonArray>();
    bool allHealthy = true;
    uint8_t n = 0;
    for (JsonObject c : arr) {
      bool healthy = c["healthy"].as<bool>();
      if (!healthy) allHealthy = false;
      if (n < Limits::MAX_CHECKS) {
        copyUtf8(work.checks[n].name, sizeof(work.checks[n].name), c["name"] | "?");
        work.checks[n].healthy = healthy;
        n++;
      }
    }
    work.checkCount       = n;
    work.checkTotal       = arr.size();
    work.allChecksHealthy = allHealthy;
    return true;
  }

  /** GET /emails → work.emails / emailSubsystemOn. Backend sorts newest first. */
  bool fetchEmails() {
    String body;
    if (!get("/emails", body)) {
      work.emailSubsystemOn = false;
      return false;
    }

    JsonDocument filter;
    filter["email_active"]              = true;
    filter["emails"][0]["id"]           = true;
    filter["emails"][0]["subject"]      = true;
    filter["emails"][0]["sender"]       = true;
    filter["emails"][0]["account"]      = true;
    filter["emails"][0]["received_at"]  = true;
    JsonDocument doc;
    DeserializationError err =
        deserializeJson(doc, body, DeserializationOption::Filter(filter));
    if (err) {
      Serial.printf("[net] emails JSON error: %s\n", err.c_str());
      work.emailSubsystemOn = false;
      return false;
    }

    work.emailSubsystemOn = doc["email_active"].as<bool>();
    JsonArray arr = doc["emails"].as<JsonArray>();
    uint8_t n = 0;
    for (JsonObject e : arr) {
      if (n >= Limits::MAX_EMAILS) break;
      EmailRec& r = work.emails[n++];
      r.id = e["id"] | -1;
      copyUtf8(r.sender,  sizeof(r.sender),  e["sender"]  | "");
      copyUtf8(r.subject, sizeof(r.subject), e["subject"] | "");
      copyUtf8(r.account, sizeof(r.account), e["account"] | "");
      formatEmailDate(e["received_at"] | "", r.date, sizeof(r.date));
    }
    work.emailCount = n;
    work.emailTotal = arr.size();
    return true;
  }

  /** Optional GET <CURRENCY_PATH> → plain text. */
  void fetchCurrency() {
    if (!Config::CURRENCY_PATH) { work.currency[0] = 0; return; }
    String body;
    if (get(Config::CURRENCY_PATH, body)) {
      // Copy manually to keep newlines (copyUtf8 turns them into spaces).
      size_t n = body.length();
      if (n >= sizeof(work.currency)) n = sizeof(work.currency) - 1;
      while (n > 0 && (body[n] & 0xC0) == 0x80) n--;
      memcpy(work.currency, body.c_str(), n);
      work.currency[n] = 0;
    }
    // on failure keep the previous text
  }

  void sendAck(const AckCommand& cmd) {
    if (cmd.kind == AckCommand::Kind::ALL) {
      post("/emails/acknowledge-all", "{}");
    } else {
      post("/emails/acknowledge", String("{\"ids\":[") + cmd.id + "]}");
    }
  }

  void publish() {
    xSemaphoreTake(gSnapMutex, portMAX_DELAY);
    gShared = work;
    gSharedVersion = gSharedVersion + 1;
    xSemaphoreGive(gSnapMutex);
  }

  void task(void*) {
    WiFi.mode(WIFI_STA);
    WiFi.setAutoReconnect(true);
    Serial.printf("[wifi] Connecting to %s\n", Config::WIFI_SSID);
    WiFi.begin(Config::WIFI_SSID, Config::WIFI_PASSWORD);
    uint32_t wifiAttemptMs = millis();
    bool     wasUp         = false;

    uint32_t lastPollMs = 0;
    bool     pollNow    = true;
    uint32_t ackSeqDone = 0;

    for (;;) {
      // ── Wi-Fi supervision ──
      bool up = (WiFi.status() == WL_CONNECTED);
      if (up != wasUp) {
        wasUp = up;
        if (up) Serial.printf("[wifi] Connected. IP: %s\n", WiFi.localIP().toString().c_str());
        else    Serial.println("[wifi] Connection lost");
        work.wifiUp = up;
        if (!up) work.backendReachable = false;
        publish();
        pollNow = up;
      }
      if (!up) {
        if (millis() - wifiAttemptMs > Config::WIFI_RETRY_MS) {
          Serial.println("[wifi] Retrying");
          WiFi.disconnect();
          WiFi.begin(Config::WIFI_SSID, Config::WIFI_PASSWORD);
          wifiAttemptMs = millis();
        }
        vTaskDelay(pdMS_TO_TICKS(250));
        continue;
      }

      // ── Acknowledge commands first, so the following poll reflects them ──
      AckCommand cmd;
      while (xQueueReceive(gAckQueue, &cmd, 0) == pdTRUE) {
        sendAck(cmd);
        ackSeqDone = cmd.seq;
        pollNow = true;
      }

      // ── Poll ──
      if (pollNow || millis() - lastPollMs >= Config::POLL_INTERVAL_MS) {
        pollNow    = false;
        lastPollMs = millis();
        bool hcOk    = fetchHealthchecks();
        bool emailOk = fetchEmails();
        fetchCurrency();
        work.backendReachable = hcOk && emailOk;
        work.polled = true;
        if (work.backendReachable) { work.hasData = true; work.lastOkMs = millis(); }
        work.ackSeq = ackSeqDone;
        Serial.printf("[poll] reachable=%d allOk=%d emailOn=%d emails=%u checks=%u\n",
                      work.backendReachable, work.allChecksHealthy,
                      work.emailSubsystemOn, work.emailTotal, work.checkTotal);
        publish();
      }

      // ── Sleep until next poll, waking early when a command is queued ──
      uint32_t elapsed = millis() - lastPollMs;
      uint32_t waitMs  = elapsed < Config::POLL_INTERVAL_MS
                         ? Config::POLL_INTERVAL_MS - elapsed : 0;
      if (waitMs > 1000) waitMs = 1000;  // re-check Wi-Fi at least once a second
      xQueuePeek(gAckQueue, &cmd, pdMS_TO_TICKS(waitMs));
    }
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  UI state (owned by loop())
// ═══════════════════════════════════════════════════════════════════════════

namespace Ui {
  Snapshot  view;                         ///< snapshot with local acks applied
  uint32_t  viewVersion   = 0;
  bool      firstSnapshot = true;

  ScreenRef screens[Limits::MAX_SCREENS];
  uint8_t   screenCount   = 0;
  uint8_t   current       = 0;

  // Acknowledged on the device but maybe not yet reflected by the backend.
  int32_t   hiddenIds[Limits::MAX_HIDDEN];
  uint8_t   hiddenCount   = 0;
  bool      hideAll       = false;        ///< ack-all sent, waiting for backend
  int32_t   hideAllMaxId  = 0;            ///< hide ids ≤ this while hideAll
  uint32_t  ackSeqSent    = 0;

  int32_t   newestSeenId  = 0;            ///< to detect newly arrived email

  bool      displayOn     = true;
  uint32_t  lastActivity  = 0;
  bool      wakeOnlyPress = false;        ///< press that woke the display: ignore it
}

LedController gLeds;
AckBlinker    gAckBlinker;
AckButton     gButton;

void applyLeds();

// ─── Display power ──────────────────────────────────────────────────────────

/** Register user activity. Returns true if the display was off (wake only). */
bool wakeDisplay() {
  Ui::lastActivity = millis();
  if (Ui::displayOn) return false;
  Ui::displayOn = true;
  u8g2.setPowerSave(0);
  return true;
}

// ─── Screen list ────────────────────────────────────────────────────────────

/** Non-empty lines in the currency text. */
uint8_t currencyLineCount() {
  uint8_t lines = 0;
  const char* p = Ui::view.currency;
  while (*p) {
    const char* nl = strchr(p, '\n');
    size_t len = nl ? (size_t)(nl - p) : strlen(p);
    if (len > 0 && !(len == 1 && p[0] == '\r')) lines++;
    if (!nl) break;
    p = nl + 1;
  }
  return lines;
}

/** Rate screens needed (at least one, for the placeholder). */
uint8_t currencyPages() {
  uint8_t pages = (currencyLineCount() + Limits::LINES_PER_PAGE - 1) / Limits::LINES_PER_PAGE;
  if (pages == 0) pages = 1;
  if (pages > Limits::MAX_RATE_PAGES) pages = Limits::MAX_RATE_PAGES;
  return pages;
}

void rebuildScreens() {
  // Remember what is on screen now, to find it again after the rebuild.
  bool      hadCurrent = Ui::screenCount > 0;
  ScreenRef cur        = hadCurrent ? Ui::screens[Ui::current] : ScreenRef{ ScreenType::EMAIL, -1 };
  uint8_t   oldIndex   = Ui::current;

  uint8_t n = 0;
  for (uint8_t i = 0; i < Ui::view.emailCount; i++)
    Ui::screens[n++] = { ScreenType::EMAIL, Ui::view.emails[i].id };

  uint8_t pages = (Ui::view.checkCount + Limits::CHECKS_PER_PAGE - 1) / Limits::CHECKS_PER_PAGE;
  if (pages == 0) pages = 1;  // always one page: shows "no checks" / "unreachable"
  for (uint8_t p = 0; p < pages; p++)
    Ui::screens[n++] = { ScreenType::HEALTH, p };

  uint8_t ratePages = currencyPages();
  for (uint8_t p = 0; p < ratePages; p++)
    Ui::screens[n++] = { ScreenType::CURRENCY, p };
  Ui::screenCount = n;

  if (!hadCurrent) { Ui::current = 0; return; }
  for (uint8_t i = 0; i < n; i++) {
    if (Ui::screens[i].type == cur.type && Ui::screens[i].key == cur.key) {
      Ui::current = i;
      return;
    }
  }
  // Current screen vanished (email acknowledged, or fewer health pages):
  // staying on the same position shows whatever moved up into its place —
  // the next email, or the first healthcheck page after the last email.
  Ui::current = oldIndex < n ? oldIndex : n - 1;
}

bool isHidden(int32_t id) {
  if (Ui::hideAll && id <= Ui::hideAllMaxId) return true;
  for (uint8_t i = 0; i < Ui::hiddenCount; i++) if (Ui::hiddenIds[i] == id) return true;
  return false;
}

/** Drop locally acknowledged emails from Ui::view. */
void filterHidden() {
  uint8_t w = 0;
  uint16_t removed = 0;
  for (uint8_t r = 0; r < Ui::view.emailCount; r++) {
    if (isHidden(Ui::view.emails[r].id)) { removed++; continue; }
    if (w != r) Ui::view.emails[w] = Ui::view.emails[r];
    w++;
  }
  Ui::view.emailCount = w;
  Ui::view.emailTotal = Ui::view.emailTotal > removed ? Ui::view.emailTotal - removed : 0;
  if (Ui::hideAll) Ui::view.emailTotal = w;  // stored list is all we know about
}

/** Pull a new snapshot from the network task, if there is one. */
void syncSnapshot() {
  if (Ui::viewVersion == gSharedVersion) return;
  xSemaphoreTake(gSnapMutex, portMAX_DELAY);
  Ui::view        = gShared;
  Ui::viewVersion = gSharedVersion;
  xSemaphoreGive(gSnapMutex);

  // Once the backend has processed every ack we sent, trust it fully.
  if (Ui::view.ackSeq >= Ui::ackSeqSent) {
    Ui::hiddenCount = 0;
    Ui::hideAll     = false;
  }
  filterHidden();

  // Wake the display when an email newer than anything seen so far arrives.
  int32_t newest = 0;
  for (uint8_t i = 0; i < Ui::view.emailCount; i++)
    if (Ui::view.emails[i].id > newest) newest = Ui::view.emails[i].id;
  if (!Ui::firstSnapshot && newest > Ui::newestSeenId) wakeDisplay();
  if (newest > Ui::newestSeenId) Ui::newestSeenId = newest;
  Ui::firstSnapshot = false;

  rebuildScreens();
  if (!gAckBlinker.running()) applyLeds();
}

// ─── Acknowledge actions ────────────────────────────────────────────────────

void onAckBlinkDone() { applyLeds(); }

void queueAck(AckCommand::Kind kind, int32_t id) {
  AckCommand cmd{ kind, id, ++Ui::ackSeqSent };
  if (xQueueSend(gAckQueue, &cmd, 0) != pdTRUE) {
    Serial.println("[ack] queue full — dropped");
    Ui::ackSeqSent--;
    return;
  }
  gAckBlinker.start(onAckBlinkDone);
}

void ackCurrentEmail() {
  const ScreenRef& s = Ui::screens[Ui::current];
  if (s.type != ScreenType::EMAIL) return;  // other screens: do nothing
  int32_t id = s.key;
  Serial.printf("[button] acknowledge email id=%d\n", (int)id);
  queueAck(AckCommand::Kind::SINGLE, id);

  // Remove locally right away; the screen then shows the next item.
  if (Ui::hiddenCount < Limits::MAX_HIDDEN) Ui::hiddenIds[Ui::hiddenCount++] = id;
  filterHidden();
  rebuildScreens();
}

void ackAllEmails() {
  if (Ui::view.emailCount == 0) return;
  Serial.println("[button] hold → acknowledge ALL emails");
  int32_t maxId = 0;
  for (uint8_t i = 0; i < Ui::view.emailCount; i++)
    if (Ui::view.emails[i].id > maxId) maxId = Ui::view.emails[i].id;
  // The backend may hold more emails than the device stores; ack-all clears
  // them all, so hide anything up to the newest id we know of.
  if (Ui::newestSeenId > maxId) maxId = Ui::newestSeenId;

  queueAck(AckCommand::Kind::ALL, 0);
  Ui::hideAll      = true;
  Ui::hideAllMaxId = maxId;
  filterHidden();
  Ui::screenCount = 0;   // forget the current screen → start from the top
  rebuildScreens();
}

// ═══════════════════════════════════════════════════════════════════════════
//  LED logic
// ═══════════════════════════════════════════════════════════════════════════

/**
 * LED_OK       ON when Wi-Fi + backend + all checks OK, PULSING otherwise
 * LED_ISSUES   PULSING on any problem, OFF when healthy
 * LED_EMAIL_OK ON when email_active is true
 * LED_EMAILS   PULSING while unacknowledged emails remain
 */
void applyLeds() {
  const Snapshot& s = Ui::view;
  bool everythingOk = s.wifiUp && s.backendReachable && s.allChecksHealthy;
  using M = LedController::Mode;
  gLeds.setMode(Pins::LED_OK,       everythingOk ? M::ON : M::PULSING);
  gLeds.setMode(Pins::LED_ISSUES,   everythingOk ? M::OFF : M::PULSING);
  gLeds.setMode(Pins::LED_EMAIL_OK, (s.wifiUp && s.emailSubsystemOn) ? M::ON : M::OFF);
  if (!gAckBlinker.running())
    gLeds.setMode(Pins::LED_EMAILS, s.emailCount > 0 ? M::PULSING : M::OFF);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Rendering — 128x64, row 0 reserved for the hold progress bar
// ═══════════════════════════════════════════════════════════════════════════

namespace Draw {
  constexpr uint8_t CHARS    = 21;        // 128 px / 6 px
  constexpr uint8_t HEADER_Y = 2;         // font top for header text
  constexpr uint8_t RULE_Y   = 14;        // separator line
  constexpr uint8_t LINE_Y[] = { 16, 28, 40, 52 };  // 4 content lines, 12 px each

  void text(uint8_t x, uint8_t y, const char* s, size_t maxChars = CHARS) {
    char buf[Limits::SUBJECT_LEN];
    fitLine(buf, sizeof(buf), s, maxChars);
    u8g2.drawUTF8(x, y, buf);
  }

  void textRight(uint8_t y, const char* s) {
    u8g2.drawUTF8(127 - u8g2.getUTF8Width(s), y, s);  // 1 px right margin
  }

  /** Title on the left, `right` aligned right, rule underneath. */
  void header(const char* title, const char* right) {
    size_t rightChars = utf8Length(right);
    size_t room = CHARS > rightChars + 1 ? CHARS - rightChars - 1 : 0;
    text(0, HEADER_Y, title, room);
    textRight(HEADER_Y, right);
    u8g2.drawHLine(0, RULE_Y, 128);
  }

  /** Word-wrap `s` over up to `lines` lines starting at LINE_Y[first]. */
  void wrapped(const char* s, uint8_t first, uint8_t lines) {
    const char* p = s;
    for (uint8_t l = 0; l < lines && *p; l++) {
      while (*p == ' ') p++;
      size_t rest = utf8Length(p);
      if (l == lines - 1 || rest <= CHARS) {   // last line: truncate with ".."
        text(0, LINE_Y[first + l], p);
        return;
      }
      // Break at the last space within CHARS, else hard-break.
      size_t limit = utf8PrefixBytes(p, CHARS);
      size_t cut = limit;
      for (size_t i = limit; i > 0; i--) if (p[i] == ' ') { cut = i; break; }
      if (cut == 0) cut = limit;
      char buf[Limits::SUBJECT_LEN];
      size_t n = cut < sizeof(buf) - 1 ? cut : sizeof(buf) - 1;
      memcpy(buf, p, n); buf[n] = 0;
      u8g2.drawUTF8(0, LINE_Y[first + l], buf);
      p += cut;
    }
  }

  void emailScreen(const ScreenRef& ref) {
    const Snapshot& s = Ui::view;
    uint8_t idx = 0;
    while (idx < s.emailCount && s.emails[idx].id != ref.key) idx++;
    if (idx >= s.emailCount) return;
    const EmailRec& e = s.emails[idx];

    char title[16];
    snprintf(title, sizeof(title), "Mail %u/%u", idx + 1, s.emailTotal);
    char account[Limits::ACCOUNT_LEN];
    fitLine(account, sizeof(account), e.account, 10);
    header(title, account);

    text(0, LINE_Y[0], e.sender[0] ? e.sender : "(no sender)");
    wrapped(e.subject[0] ? e.subject : "(no subject)", 1, 2);

    char foot[24];
    if (idx == s.emailCount - 1 && s.emailTotal > s.emailCount)
      snprintf(foot, sizeof(foot), "%s  +%u more", e.date, s.emailTotal - s.emailCount);
    else
      snprintf(foot, sizeof(foot), "%s", e.date);
    text(0, LINE_Y[3], foot);
  }

  void healthScreen(const ScreenRef& ref) {
    const Snapshot& s = Ui::view;
    uint8_t pages = (s.checkCount + Limits::CHECKS_PER_PAGE - 1) / Limits::CHECKS_PER_PAGE;
    if (pages == 0) pages = 1;

    uint8_t ok = 0;
    for (uint8_t i = 0; i < s.checkCount; i++) if (s.checks[i].healthy) ok++;

    char title[24], right[16];
    if (pages > 1) snprintf(title, sizeof(title), "Health %ld/%u", (long)ref.key + 1, pages);
    else           snprintf(title, sizeof(title), "Health");
    snprintf(right, sizeof(right), "%u/%u OK", ok, s.checkCount);
    header(title, s.checkCount ? right : "");

    if (!s.wifiUp) {
      text(0, LINE_Y[0], "Wi-Fi disconnected");
      text(0, LINE_Y[1], Config::WIFI_SSID);
      return;
    }
    if (!s.polled) {
      text(0, LINE_Y[0], "Polling backend...");
      return;
    }
    if (!s.backendReachable) {
      text(0, LINE_Y[0], "Backend unreachable");
      if (s.hasData) {
        char buf[24];
        snprintf(buf, sizeof(buf), "Last OK %lus ago",
                 (unsigned long)((millis() - s.lastOkMs) / 1000));
        text(0, LINE_Y[1], buf);
      }
      return;
    }
    if (s.checkCount == 0) {
      text(0, LINE_Y[0], "No healthchecks");
      text(0, LINE_Y[1], "configured");
      return;
    }

    uint8_t start = ref.key * Limits::CHECKS_PER_PAGE;
    for (uint8_t l = 0; l < Limits::CHECKS_PER_PAGE; l++) {
      uint8_t i = start + l;
      if (i >= s.checkCount) break;
      const CheckRec& c = s.checks[i];
      uint8_t y = LINE_Y[l];
      if (c.healthy) {
        text(0, y, c.name, CHARS - 3);
        textRight(y, "OK");
      } else {
        // Failing service: inverted line so it stands out.
        u8g2.drawBox(0, y - 1, 128, 12);
        u8g2.setDrawColor(0);
        text(1, y, c.name, CHARS - 5);
        textRight(y, "FAIL");
        u8g2.setDrawColor(1);
      }
    }
  }

  void currencyScreen(const ScreenRef& ref) {
    uint8_t pages = currencyPages();
    char title[24];
    if (pages > 1) snprintf(title, sizeof(title), "Rates %ld/%u", (long)ref.key + 1, pages);
    else           snprintf(title, sizeof(title), "Rates");
    header(title, "");
    const char* t = Ui::view.currency;
    if (!t[0]) {
      text(0, LINE_Y[0], "No data yet");
      text(0, LINE_Y[1], "(backend support");
      text(0, LINE_Y[2], " pending)");
      return;
    }
    // Plain text from the backend, one entry per line; skip earlier pages.
    char line[Limits::SUBJECT_LEN];
    uint16_t first = ref.key * Limits::LINES_PER_PAGE;
    uint16_t idx = 0;
    uint8_t  l = 0;
    const char* p = t;
    while (*p && l < Limits::LINES_PER_PAGE) {
      const char* nl = strchr(p, '\n');
      size_t n = nl ? (size_t)(nl - p) : strlen(p);
      if (n >= sizeof(line)) n = sizeof(line) - 1;
      memcpy(line, p, n); line[n] = 0;
      if (n && line[n - 1] == '\r') line[--n] = 0;
      if (n > 0) {
        if (idx >= first) text(0, LINE_Y[l++], line);
        idx++;
      }
      if (!nl) break;
      p = nl + 1;
    }
  }

  void bootScreen() {
    header("healthmon", "v1.2");
    text(0, LINE_Y[0], Ui::view.wifiUp ? "Polling backend..." : "Connecting Wi-Fi:");
    if (!Ui::view.wifiUp) text(0, LINE_Y[1], Config::WIFI_SSID);
  }

  /** Hold progress bar on pixel row 0 — only when a hold would do something. */
  void holdBar() {
    if (!gButton.isHeld() || gButton.holdFired() || Ui::wakeOnlyPress) return;
    if (Ui::view.emailCount == 0) return;
    uint32_t held = gButton.heldFor();
    uint16_t w = (uint32_t)128 * held / Config::ACK_ALL_HOLD_MS;
    if (w > 128) w = 128;
    if (w) u8g2.drawHLine(0, 0, w);
  }

  void frame() {
    u8g2.clearBuffer();
    u8g2.setFont(FONT_MAIN);
    u8g2.setFontPosTop();
    u8g2.setDrawColor(1);

    if (Ui::firstSnapshot || Ui::screenCount == 0) {
      bootScreen();
    } else if (Ui::screenCount) {
      const ScreenRef& ref = Ui::screens[Ui::current];
      switch (ref.type) {
        case ScreenType::EMAIL:    emailScreen(ref);  break;
        case ScreenType::HEALTH:   healthScreen(ref); break;
        case ScreenType::CURRENCY: currencyScreen(ref); break;
      }
    }
    holdBar();
    u8g2.sendBuffer();
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Self-test
// ═══════════════════════════════════════════════════════════════════════════

/** Flash all LEDs in sequence on startup (delay() is fine in setup only). */
void selfTest() {
  const uint8_t pins[] = { Pins::LED_OK, Pins::LED_ISSUES, Pins::LED_EMAIL_OK, Pins::LED_EMAILS };
  for (uint8_t pin : pins) { ledcWrite(pin, Config::PWM_MAX); delay(200); ledcWrite(pin, 0); }
  for (uint8_t pin : pins) ledcWrite(pin, Config::PWM_MAX);
  delay(400);
  for (uint8_t pin : pins) ledcWrite(pin, 0);
  delay(200);
}

// ═══════════════════════════════════════════════════════════════════════════
//  setup()
// ═══════════════════════════════════════════════════════════════════════════

void setup() {
  Serial.begin(115200);
  delay(500);  // brief settle for USB-CDC
  Serial.println("[healthmon-device] v1.2 starting up");

  Wire.begin(Pins::DISPLAY_SDA, Pins::DISPLAY_SCL);
  u8g2.begin();
  u8g2.setContrast(Config::DISPLAY_CONTRAST);
  u8g2.enableUTF8Print();
  Draw::frame();  // boot screen during the LED self-test

  gLeds.begin();
  selfTest();

  gButton.begin(Pins::BUTTON);
  Encoder::begin();

  gSnapMutex = xSemaphoreCreateMutex();
  gAckQueue  = xQueueCreate(8, sizeof(AckCommand));
  // Same priority as loop(): FreeRTOS time-slices, and HTTP waits yield.
  xTaskCreate(Net::task, "net", 12288, nullptr, 1, nullptr);

  // Wi-Fi not up yet: pulse LED_OK, like v1.1 did while connecting.
  gLeds.setMode(Pins::LED_OK, LedController::Mode::PULSING);
  Ui::lastActivity = millis();
}

// ═══════════════════════════════════════════════════════════════════════════
//  loop() — UI only, never blocks
// ═══════════════════════════════════════════════════════════════════════════

void loop() {
  static uint32_t lastRender = 0;
  uint32_t now = millis();

  // ── 1. LEDs ──
  gAckBlinker.update();
  gLeds.update(gAckBlinker.running());

  // ── 2. New data from the network task ──
  syncSnapshot();

  // ── 3. Encoder: one click = one screen ──
  int32_t detents = Encoder::takeDetents();
  if (detents != 0) {
    bool wasOff = wakeDisplay();
    if (!wasOff && Ui::screenCount > 0) {
      int32_t next = ((int32_t)Ui::current + detents) % Ui::screenCount;
      if (next < 0) next += Ui::screenCount;
      Ui::current = (uint8_t)next;
    }
  }

  // ── 4. Acknowledge button ──
  AckButton::Events ev = gButton.update();
  if (ev.pressed) Ui::wakeOnlyPress = wakeDisplay();
  if (ev.holdDone && !Ui::wakeOnlyPress) ackAllEmails();
  if (ev.shortPress && !Ui::wakeOnlyPress && Ui::screenCount > 0) ackCurrentEmail();
  if (gButton.isHeld()) Ui::lastActivity = now;  // don't blank mid-hold
  if (!gButton.isHeld()) Ui::wakeOnlyPress = false;

  // ── 5. Display timeout ──
  if (Config::DISPLAY_TIMEOUT_MS > 0 && Ui::displayOn &&
      now - Ui::lastActivity > Config::DISPLAY_TIMEOUT_MS) {
    Ui::displayOn = false;
    u8g2.setPowerSave(1);
  }

  // ── 6. Render ──
  if (Ui::displayOn && now - lastRender >= Config::RENDER_INTERVAL_MS) {
    lastRender = now;
    Draw::frame();
  }

  delay(1);  // let the idle task run (feeds the task watchdog)
}
