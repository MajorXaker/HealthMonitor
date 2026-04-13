/**
 * healthmon_device.ino
 *
 * Hardware status indicator for the healthmon backend.
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
 * - One button: press to acknowledge all emails (POST /emails/acknowledge-all).
 *   On press: LED_EMAILS blinks 3× quickly, then turns off.
 * - No blocking delay() calls in the main loop.
 * - PWM pulsing via ESP32 LEDC hardware peripheral.
 * - Configurable steady brightness to reduce eye strain on bright LEDs.
 *
 * Wiring (see wiring document)
 * ----------------------------
 * D0  → LED_OK       anode (via 220 Ω resistor to GND)
 * D1  → LED_ISSUES   anode (via 220 Ω resistor to GND)
 * D2  → LED_EMAIL_OK anode (via 220 Ω resistor to GND)
 * D3  → LED_EMAILS   anode (via 220 Ω resistor to GND)
 * D10 → Button (other leg to GND, internal pull-up enabled)
 *
 * Arduino IDE setup
 * -----------------
 * 1. File → Preferences → Additional boards manager URLs:
 *    https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json
 * 2. Tools → Board → Boards Manager → search "esp32" → install "esp32 by Espressif Systems" (≥3.x)
 * 3. Tools → Board → ESP32 Arduino → "XIAO_ESP32C6"
 * 4. Tools → Port → select the USB-CDC port
 * 5. Sketch → Upload
 *
 * Libraries required:
 *   WiFi.h, HTTPClient.h — bundled with ESP32 Arduino core
 *   base64.h             — bundled with ESP32 Arduino core
 *   ArduinoJson          — install via Library Manager (Benoit Blanchon, ≥7.x)
 */

// ─── Dependencies ────────────────────────────────────────────────────────────
#include <WiFi.h>
#include <HTTPClient.h>
#include <ArduinoJson.h>
#include <base64.h>  // ESP32 Arduino core built-in

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

  // Polling interval for healthcheck + email status (milliseconds)
  constexpr unsigned long POLL_INTERVAL_MS = 30000;

  // HTTP request timeout (milliseconds)
  constexpr int HTTP_TIMEOUT_MS = 8000;

  // Button debounce window (milliseconds)
  constexpr unsigned long DEBOUNCE_MS = 50;

  // PWM pulsing: period for one full bright→dim→bright cycle (milliseconds)
  constexpr unsigned long PULSE_PERIOD_MS = 1500;

  // LEDC PWM settings
  constexpr uint32_t PWM_FREQ  = 5000;  // Hz
  constexpr uint8_t  PWM_BITS  = 8;     // resolution: 0–255
  constexpr uint8_t  PWM_MAX   = 255;
  constexpr uint8_t  PWM_MIN   = 5;     // floor during pulsing (never fully off)

  // Steady-on brightness (0–255).
  // Reduce this if your LEDs are too bright in the ON state.
  // 255 = full brightness, 60 ≈ 25%, 30 ≈ 12%.
  constexpr uint8_t  PWM_STEADY = 15;

  // Acknowledge blink animation: 3 quick flashes on LED_EMAILS after button press.
  constexpr uint8_t  ACK_BLINK_COUNT    = 3;
  constexpr uint32_t ACK_BLINK_ON_MS    = 80;   // LED on duration per blink
  constexpr uint32_t ACK_BLINK_OFF_MS   = 100;  // LED off gap between blinks
}

// ─── Pin assignments ──────────────────────────────────────────────────────────
namespace Pins {
  constexpr uint8_t LED_OK       = D0;   // "all is ok" (green/blue)
  constexpr uint8_t LED_ISSUES   = D1;   // "healthcheck issues" (red)
  constexpr uint8_t LED_EMAIL_OK = D2;   // "email subsystem online" (green/blue)
  constexpr uint8_t LED_EMAILS   = D3;   // "new emails" (yellow)
  constexpr uint8_t BUTTON       = D10;  // acknowledge emails
}

// ─── Forward declarations ─────────────────────────────────────────────────────
class LedController;
class ButtonHandler;
struct AppState;

void connectWifi();
void pollBackend(AppState& state);
bool fetchHealthchecks(AppState& state);
bool fetchEmails(AppState& state);
void acknowledgeAllEmails();
void applyLeds(const AppState& state, LedController& leds);
String buildAuthHeader();

// ═══════════════════════════════════════════════════════════════════════════
//  AckBlinker
//  Non-blocking 3-flash animation played on LED_EMAILS after acknowledge.
//  State machine: drives the LED through ON/OFF phases using millis(),
//  then fires a completion callback when all blinks are done.
// ═══════════════════════════════════════════════════════════════════════════
class AckBlinker {
public:
  using DoneCallback = void (*)();

  /** Start the blink sequence. Safe to call even if already running (restarts). */
  void start(DoneCallback onDone) {
    _onDone    = onDone;
    _blinks    = 0;
    _phase     = Phase::ON;
    _phaseStart = millis();
    _running   = true;
    ledcWrite(Pins::LED_EMAILS, Config::PWM_MAX);  // first blink on immediately
  }

  /** Returns true while the animation is in progress. */
  bool running() const { return _running; }

  /**
   * Advance the state machine. Must be called every loop() iteration.
   * Drives LED_EMAILS independently of LedController while active.
   */
  void update() {
    if (!_running) return;

    uint32_t now     = millis();
    uint32_t elapsed = now - _phaseStart;

    if (_phase == Phase::ON && elapsed >= Config::ACK_BLINK_ON_MS) {
      // End of ON phase → turn off, move to OFF gap
      ledcWrite(Pins::LED_EMAILS, 0);
      _blinks++;
      if (_blinks >= Config::ACK_BLINK_COUNT) {
        // All blinks done
        _running = false;
        if (_onDone) _onDone();
      } else {
        _phase      = Phase::OFF;
        _phaseStart = now;
      }
    } else if (_phase == Phase::OFF && elapsed >= Config::ACK_BLINK_OFF_MS) {
      // End of OFF gap → start next blink
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
//  Manages four LEDs. Each LED can be in one of three modes:
//    OFF     – duty 0
//    ON      – duty PWM_STEADY (configurable brightness, not always full)
//    PULSING – duty oscillates sinusoidally between PWM_MIN and PWM_MAX
// ═══════════════════════════════════════════════════════════════════════════
class LedController {
public:
  enum class Mode { OFF, ON, PULSING };

  /**
   * Attach LEDC PWM channels to all four LED pins.
   * Must be called once in setup().
   */
  void begin() {
    ledcAttach(Pins::LED_OK,       Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_ISSUES,   Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAIL_OK, Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAILS,   Config::PWM_FREQ, Config::PWM_BITS);
  }

  /**
   * Set the desired mode for a specific LED pin.
   * ON uses PWM_STEADY so brightness is adjustable without changing wiring.
   */
  void setMode(uint8_t pin, Mode mode) {
    getSlot(pin).mode = mode;
    if (mode == Mode::OFF)  ledcWrite(pin, 0);
    uint32_t brightness = 255;
    if (mode == Mode::ON)   {
      if (pin == Pins::LED_EMAIL_OK)  brightness = Config::PWM_STEADY;
      ledcWrite(pin, brightness);
    };
    // PULSING duty is updated each tick in update()
  }

  /**
   * Update PWM duty cycles for pulsing LEDs.
   * Call this every loop() iteration; it is non-blocking.
   * Skips LED_EMAILS if the AckBlinker is currently running (it owns that pin).
   *
   * @param skipEmailsPin  Pass true while AckBlinker is active.
   */
  void update(bool skipEmailsPin = false) {
    uint32_t now   = millis();
    uint32_t phase = now % Config::PULSE_PERIOD_MS;
    float    t     = (float)phase / (float)Config::PULSE_PERIOD_MS;
    float    sine  = (sinf(t * 2.0f * PI) + 1.0f) / 2.0f;  // [0, 1]
    uint8_t  duty  = (uint8_t)(Config::PWM_MIN + sine * (Config::PWM_MAX - Config::PWM_MIN));

    for (auto& slot : _slots) {
      if (slot.mode == Mode::PULSING) {
        if (skipEmailsPin && slot.pin == Pins::LED_EMAILS) continue;
        ledcWrite(slot.pin, duty);
      }
    }
  }

private:
  struct Slot {
    uint8_t pin;
    Mode    mode = Mode::OFF;
  };

  Slot _slots[4] = {
    { Pins::LED_OK       },
    { Pins::LED_ISSUES   },
    { Pins::LED_EMAIL_OK },
    { Pins::LED_EMAILS   },
  };

  Slot& getSlot(uint8_t pin) {
    for (auto& s : _slots) if (s.pin == pin) return s;
    return _slots[0];
  }
};

// ═══════════════════════════════════════════════════════════════════════════
//  ButtonHandler
//  Debounces the acknowledge button and fires a callback on a clean press.
//  Uses no delay(); pure state-machine approach.
// ═══════════════════════════════════════════════════════════════════════════
class ButtonHandler {
public:
  using Callback = void (*)();

  /**
   * @param pin      GPIO pin (active-LOW, INPUT_PULLUP)
   * @param callback Called once per confirmed press
   */
  void begin(uint8_t pin, Callback callback) {
    _pin      = pin;
    _callback = callback;
    pinMode(pin, INPUT_PULLUP);
  }

  /** Poll button state. Must be called every loop() iteration. */
  void update() {
    bool reading = (digitalRead(_pin) == LOW);
    uint32_t now = millis();

    if (reading != _lastReading) {
      _debounceStart = now;
      _lastReading   = reading;
    }

    if ((now - _debounceStart) >= Config::DEBOUNCE_MS) {
      if (reading && !_confirmed) {
        _confirmed = true;
        if (_callback) _callback();
      }
      if (!reading) {
        _confirmed = false;
      }
    }
  }

private:
  uint8_t  _pin           = 0;
  Callback _callback      = nullptr;
  bool     _lastReading   = false;
  bool     _confirmed     = false;
  uint32_t _debounceStart = 0;
};

// ═══════════════════════════════════════════════════════════════════════════
//  AppState
//  Plain data struct holding the latest observed state from the backend.
// ═══════════════════════════════════════════════════════════════════════════
struct AppState {
  bool backendReachable  = false;  ///< Last HTTP poll succeeded
  bool allChecksHealthy  = false;  ///< No failing healthchecks
  bool emailSubsystemOn  = false;  ///< email_active flag from /emails response
  bool hasNewEmails      = false;  ///< emails array is non-empty
};

// ─── Globals ─────────────────────────────────────────────────────────────────
LedController  gLeds;
ButtonHandler  gButton;
AckBlinker     gAckBlinker;
AppState       gState;

unsigned long  gLastPollMs = 0;
bool           gAckPending = false;  ///< Set by button callback, consumed in loop()

// ─── Callbacks ───────────────────────────────────────────────────────────────

/** Called by ButtonHandler on confirmed press. */
void onButtonPress() {
  gAckPending = true;
  Serial.println("[button] ACK pressed");
}

/**
 * Called by AckBlinker when the 3-blink animation finishes.
 * Restores LED_EMAILS to OFF (acknowledge has cleared the emails).
 */
void onAckBlinkDone() {
  gLeds.setMode(Pins::LED_EMAILS, LedController::Mode::OFF);
}

// ═══════════════════════════════════════════════════════════════════════════
//  setup()
// ═══════════════════════════════════════════════════════════════════════════
void setup() {
  Serial.begin(115200);
  delay(500);  // brief settle for USB-CDC

  Serial.println("[healthmon-device] Starting up");

  gLeds.begin();
  selfTest();

  gButton.begin(Pins::BUTTON, onButtonPress);

  connectWifi();

  // Trigger an immediate first poll
  gLastPollMs = millis() - Config::POLL_INTERVAL_MS;
}

// ═══════════════════════════════════════════════════════════════════════════
//  loop()
// ═══════════════════════════════════════════════════════════════════════════
void loop() {
  uint32_t now = millis();

  // ── 1. Advance blink animation (owns LED_EMAILS pin while running) ──────
  gAckBlinker.update();

  // ── 2. Update LED pulsing (skip LED_EMAILS if blinker is active) ────────
  gLeds.update(gAckBlinker.running());

  // ── 3. Check button ──────────────────────────────────────────────────────
  gButton.update();

  // ── 4. Handle pending acknowledge ───────────────────────────────────────
  if (gAckPending) {
    gAckPending = false;
    Serial.println("[button] Acknowledge all emails requested");

    // Start 3-blink animation immediately (non-blocking).
    // The HTTP request fires after, so visual feedback is instant.
    gAckBlinker.start(onAckBlinkDone);

    acknowledgeAllEmails();

    // Force an immediate re-poll to refresh LED state after acknowledging
    gLastPollMs = now - Config::POLL_INTERVAL_MS;
  }

  // ── 5. Reconnect Wi-Fi if lost ───────────────────────────────────────────
  if (WiFi.status() != WL_CONNECTED) {
    Serial.println("[wifi] Connection lost – reconnecting");
    gState.backendReachable = false;
    applyLeds(gState, gLeds);
    connectWifi();
    return;
  }

  // ── 6. Poll backend on schedule ──────────────────────────────────────────
  if (now - gLastPollMs >= Config::POLL_INTERVAL_MS) {
    gLastPollMs = now;
    pollBackend(gState);
    applyLeds(gState, gLeds);
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Wi-Fi
// ═══════════════════════════════════════════════════════════════════════════

/** Block until Wi-Fi is connected. Pulses LED_OK while waiting. */
void connectWifi() {
  Serial.printf("[wifi] Connecting to %s\n", Config::WIFI_SSID);
  WiFi.mode(WIFI_STA);
  WiFi.begin(Config::WIFI_SSID, Config::WIFI_PASSWORD);

  gLeds.setMode(Pins::LED_OK,       LedController::Mode::PULSING);
  gLeds.setMode(Pins::LED_ISSUES,   LedController::Mode::OFF);
  gLeds.setMode(Pins::LED_EMAIL_OK, LedController::Mode::OFF);
  gLeds.setMode(Pins::LED_EMAILS,   LedController::Mode::OFF);

  uint32_t timeout = millis() + 20000;
  while (WiFi.status() != WL_CONNECTED && millis() < timeout) {
    gLeds.update();
    delay(10);
  }

  if (WiFi.status() == WL_CONNECTED) {
    Serial.printf("[wifi] Connected. IP: %s\n", WiFi.localIP().toString().c_str());
    gLeds.setMode(Pins::LED_OK, LedController::Mode::OFF);
  } else {
    Serial.println("[wifi] Failed to connect – will retry next loop");
    gLeds.setMode(Pins::LED_ISSUES, LedController::Mode::PULSING);
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Backend polling
// ═══════════════════════════════════════════════════════════════════════════

/** Build a Basic Auth header value from the configured credentials. */
String buildAuthHeader() {
  return "Basic " + base64::encode(String(Config::BASIC_AUTH));
}

/**
 * Poll both /healthchecks and /emails, update AppState.
 * Sets backendReachable=false if either request fails.
 */
void pollBackend(AppState& state) {
  Serial.println("[poll] Polling backend");
  bool hcOk    = fetchHealthchecks(state);
  bool emailOk = fetchEmails(state);
  state.backendReachable = hcOk && emailOk;
  Serial.printf("[poll] reachable=%d allOk=%d emailOn=%d newEmails=%d\n",
    state.backendReachable, state.allChecksHealthy,
    state.emailSubsystemOn, state.hasNewEmails);
}

/**
 * GET /healthchecks
 * Sets state.allChecksHealthy = true only if ALL entries report healthy=true.
 * Returns true if the HTTP request succeeded.
 */
bool fetchHealthchecks(AppState& state) {
  HTTPClient http;
  String url = String(Config::BACKEND_HOST) + "/healthchecks";
  http.begin(url);
  http.addHeader("Authorization", buildAuthHeader());
  http.setTimeout(Config::HTTP_TIMEOUT_MS);

  int code = http.GET();
  if (code != 200) {
    Serial.printf("[healthcheck] HTTP %d\n", code);
    http.end();
    state.allChecksHealthy = false;
    return false;
  }

  JsonDocument doc;
  DeserializationError err = deserializeJson(doc, http.getString());
  http.end();

  if (err) {
    Serial.printf("[healthcheck] JSON parse error: %s\n", err.c_str());
    state.allChecksHealthy = false;
    return false;
  }

  bool allHealthy = true;
  for (JsonObject check : doc.as<JsonArray>()) {
    if (!check["healthy"].as<bool>()) {
      allHealthy = false;
      break;
    }
  }
  state.allChecksHealthy = allHealthy;
  return true;
}

/**
 * GET /emails
 *
 * Response shape (from backend):
 *   { "email_active": bool, "emails": [...] }
 *
 * - state.emailSubsystemOn = value of the top-level "email_active" field.
 * - state.hasNewEmails     = true if the "emails" array is non-empty.
 *
 * Returns true if the HTTP request succeeded.
 */
bool fetchEmails(AppState& state) {
  HTTPClient http;
  String url = String(Config::BACKEND_HOST) + "/emails";
  http.begin(url);
  http.addHeader("Authorization", buildAuthHeader());
  http.setTimeout(Config::HTTP_TIMEOUT_MS);

  int code = http.GET();
  if (code != 200) {
    Serial.printf("[emails] HTTP %d\n", code);
    http.end();
    state.emailSubsystemOn = false;
    state.hasNewEmails     = false;
    return false;
  }

  JsonDocument doc;
  DeserializationError err = deserializeJson(doc, http.getString());
  http.end();

  if (err) {
    Serial.printf("[emails] JSON parse error: %s\n", err.c_str());
    state.emailSubsystemOn = false;
    state.hasNewEmails     = false;
    return false;
  }

  // Read the top-level "email_active" boolean flag.
  state.emailSubsystemOn = doc["email_active"].as<bool>();

  // Count entries in the "emails" array.
  state.hasNewEmails = (doc["emails"].as<JsonArray>().size() > 0);

  return true;
}

/**
 * POST /emails/acknowledge-all
 * Fires on button press. Does not block the main loop beyond the HTTP round trip.
 */
void acknowledgeAllEmails() {
  if (WiFi.status() != WL_CONNECTED) {
    Serial.println("[ack] No Wi-Fi – cannot send acknowledge");
    return;
  }

  HTTPClient http;
  String url = String(Config::BACKEND_HOST) + "/emails/acknowledge-all";
  http.begin(url);
  http.addHeader("Authorization", buildAuthHeader());
  http.addHeader("Content-Type", "application/json");
  http.setTimeout(Config::HTTP_TIMEOUT_MS);

  int code = http.POST("{}");
  Serial.printf("[ack] POST /emails/acknowledge-all → HTTP %d\n", code);
  http.end();
}

// ═══════════════════════════════════════════════════════════════════════════
//  LED logic
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Translate AppState into LED modes.
 *
 * LED_OK (green/blue)
 *   ON      — Wi-Fi up AND backend reachable AND all healthchecks healthy
 *   PULSING — any of the above is false
 *
 * LED_ISSUES (red)
 *   PULSING — backend unreachable or at least one healthcheck failing
 *   OFF     — everything healthy
 *
 * LED_EMAIL_OK (green/blue)
 *   ON  — email_active is true (backend has email accounts configured + polling)
 *   OFF — email_active is false
 *
 * LED_EMAILS (yellow)
 *   PULSING — new unacknowledged emails present
 *   OFF     — no new emails
 *   (overridden by AckBlinker for a short 3-blink animation on button press)
 */
void applyLeds(const AppState& state, LedController& leds) {
  // Don't touch LED_EMAILS if the ack blink animation is in progress.
  bool blinkActive = gAckBlinker.running();

  bool wifiOk       = (WiFi.status() == WL_CONNECTED);
  bool everythingOk = wifiOk && state.backendReachable && state.allChecksHealthy;

  leds.setMode(Pins::LED_OK,
    everythingOk ? LedController::Mode::ON : LedController::Mode::PULSING);

  leds.setMode(Pins::LED_ISSUES,
    everythingOk ? LedController::Mode::OFF : LedController::Mode::PULSING);

  leds.setMode(Pins::LED_EMAIL_OK,
    state.emailSubsystemOn ? LedController::Mode::ON : LedController::Mode::OFF);

  if (!blinkActive) {
    leds.setMode(Pins::LED_EMAILS,
      state.hasNewEmails ? LedController::Mode::PULSING : LedController::Mode::OFF);
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Self-test
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Flash all LEDs in sequence on startup to confirm wiring and PWM work.
 * Uses delay() — only during setup, never in the main loop.
 */
void selfTest() {
  const uint8_t pins[] = {
    Pins::LED_OK, Pins::LED_ISSUES, Pins::LED_EMAIL_OK, Pins::LED_EMAILS
  };
  for (uint8_t pin : pins) {
    ledcWrite(pin, Config::PWM_MAX);
    delay(200);
    ledcWrite(pin, 0);
  }
  for (uint8_t pin : pins) ledcWrite(pin, Config::PWM_MAX);
  delay(400);
  for (uint8_t pin : pins) ledcWrite(pin, 0);
  delay(200);
}
