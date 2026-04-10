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
 * - No blocking delay() calls in the main loop.
 * - PWM pulsing via ESP32 LEDC hardware peripheral.
 *
 * Wiring (see wiring document)
 * ----------------------------
 * D2  → LED_OK       anode (via 220 Ω resistor to GND)
 * D3  → LED_ISSUES   anode (via 220 Ω resistor to GND)
 * D9  → LED_EMAIL_OK anode (via 220 Ω resistor to GND)
 * D10 → LED_EMAILS   anode (via 220 Ω resistor to GND)
 * D1  → Button (other leg to GND, internal pull-up enabled)
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
 * Libraries required (all bundled with the ESP32 Arduino core, no extras needed):
 *   WiFi.h, HTTPClient.h, ArduinoJson.h
 *
 * ArduinoJson must be installed separately:
 *   Sketch → Include Library → Manage Libraries → search "ArduinoJson" by Benoit Blanchon → install ≥7.x
 */

// ─── Dependencies ────────────────────────────────────────────────────────────
#include <WiFi.h>
#include <HTTPClient.h>
#include <ArduinoJson.h>

// ═══════════════════════════════════════════════════════════════════════════
//  CONFIGURATION — edit these values before flashing
//  All settings are baked into the firmware; a config change requires reflash.
// ═══════════════════════════════════════════════════════════════════════════

namespace Config {
  // Wi-Fi credentials
  constexpr const char* WIFI_SSID     = "YOUR_WIFI_SSID";
  constexpr const char* WIFI_PASSWORD = "YOUR_WIFI_PASSWORD";

  // healthmon backend
  constexpr const char* BACKEND_HOST  = "http://192.168.1.100:8080";
  constexpr const char* BASIC_AUTH    = "admin:changeme";  // user:password

  // Polling interval for healthcheck + email status (milliseconds)
  constexpr unsigned long POLL_INTERVAL_MS = 15000;

  // HTTP request timeout (milliseconds)
  constexpr int HTTP_TIMEOUT_MS = 8000;

  // Button debounce window (milliseconds)
  constexpr unsigned long DEBOUNCE_MS = 50;

  // PWM pulsing: period for one full bright→dim→bright cycle (milliseconds)
  constexpr unsigned long PULSE_PERIOD_MS = 1500;

  // LEDC PWM settings
  constexpr uint32_t PWM_FREQ      = 5000;  // Hz
  constexpr uint8_t  PWM_BITS      = 8;     // resolution: 0-255
  constexpr uint8_t  PWM_MAX       = 255;
  constexpr uint8_t  PWM_MIN       = 5;     // never fully off during pulse
}

// ─── Pin assignments ──────────────────────────────────────────────────────────
namespace Pins {
  constexpr uint8_t LED_OK       = D2;   // GPIO2  – "all is ok" (green/blue)
  constexpr uint8_t LED_ISSUES   = D3;   // GPIO21 – "healthcheck issues" (red)
  constexpr uint8_t LED_EMAIL_OK = D9;   // GPIO20 – "email subsystem online" (green/blue)
  constexpr uint8_t LED_EMAILS   = D10;  // GPIO18 – "new emails" (yellow)
  constexpr uint8_t BUTTON       = D1;   // GPIO1  – acknowledge emails
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
//  LedController
//  Manages four LEDs. Each LED can be in one of three modes:
//    OFF     – duty 0
//    ON      – duty max (fully lit)
//    PULSING – duty oscillates sinusoidally between PWM_MIN and PWM_MAX
// ═══════════════════════════════════════════════════════════════════════════
class LedController {
public:
  enum class Mode { OFF, ON, PULSING };

  /**
   * Attach LEDC PWM to all four LED pins.
   * Must be called once in setup().
   */
  void begin() {
    ledcAttach(Pins::LED_OK,       Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_ISSUES,   Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAIL_OK, Config::PWM_FREQ, Config::PWM_BITS);
    ledcAttach(Pins::LED_EMAILS,   Config::PWM_FREQ, Config::PWM_BITS);
  }

  /** Set the desired mode for a specific LED pin. */
  void setMode(uint8_t pin, Mode mode) {
    getSlot(pin).mode = mode;
    if (mode == Mode::OFF)  ledcWrite(pin, 0);
    if (mode == Mode::ON)   ledcWrite(pin, Config::PWM_MAX);
  }

  /**
   * Update PWM duty cycles for pulsing LEDs.
   * Call this every loop() iteration; it is non-blocking.
   */
  void update() {
    uint32_t now = millis();
    // Compute phase in [0, PULSE_PERIOD_MS)
    uint32_t phase = now % Config::PULSE_PERIOD_MS;
    // Map phase to a sine wave in [0, PI] → value in [0, 1]
    float t       = (float)phase / (float)Config::PULSE_PERIOD_MS; // [0,1)
    float sine    = (sinf(t * 2.0f * PI) + 1.0f) / 2.0f;           // [0,1]
    uint8_t duty  = (uint8_t)(Config::PWM_MIN + sine * (Config::PWM_MAX - Config::PWM_MIN));

    for (auto& slot : _slots) {
      if (slot.mode == Mode::PULSING) {
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
    return _slots[0]; // fallback (shouldn't happen)
  }
};

// ═══════════════════════════════════════════════════════════════════════════
//  ButtonHandler
//  Debounces the acknowledge button and fires a callback on a clean press.
//  Uses no delay(); state machine approach.
// ═══════════════════════════════════════════════════════════════════════════
class ButtonHandler {
public:
  using Callback = void (*)();

  /**
   * @param pin      GPIO pin of the button (active-LOW with INPUT_PULLUP)
   * @param callback Function to call on a confirmed button press
   */
  void begin(uint8_t pin, Callback callback) {
    _pin      = pin;
    _callback = callback;
    pinMode(pin, INPUT_PULLUP);
  }

  /**
   * Poll the button state. Must be called every loop() iteration.
   * Fires the callback once per physical press after debounce settles.
   */
  void update() {
    bool reading = (digitalRead(_pin) == LOW); // active-LOW
    uint32_t now = millis();

    if (reading != _lastReading) {
      // Edge detected – start/reset debounce timer
      _debounceStart = now;
      _lastReading   = reading;
    }

    if ((now - _debounceStart) >= Config::DEBOUNCE_MS) {
      // Signal has been stable for the debounce window
      if (reading && !_confirmed) {
        // Stable press – fire once
        _confirmed = true;
        if (_callback) _callback();
      }
      if (!reading) {
        // Button released – allow next press
        _confirmed = false;
      }
    }
  }

private:
  uint8_t    _pin           = 0;
  Callback   _callback      = nullptr;
  bool       _lastReading   = false;
  bool       _confirmed     = false;
  uint32_t   _debounceStart = 0;
};

// ═══════════════════════════════════════════════════════════════════════════
//  AppState
//  Plain data struct holding the latest observed state of the backend.
// ═══════════════════════════════════════════════════════════════════════════
struct AppState {
  bool backendReachable  = false;  ///< Last HTTP poll succeeded
  bool allChecksHealthy  = false;  ///< No failing healthchecks
  bool emailSubsystemOn  = false;  ///< At least one email account configured
  bool hasNewEmails      = false;  ///< Unacknowledged emails exist
};

// ─── Globals ─────────────────────────────────────────────────────────────────
LedController  gLeds;
ButtonHandler  gButton;
AppState       gState;

unsigned long  gLastPollMs = 0;
bool           gAckPending = false;  ///< Set by button ISR, consumed in loop()

// ─── Button callback (called from ButtonHandler, safe context) ────────────────
void onButtonPress() {
  gAckPending = true;
}

// ═══════════════════════════════════════════════════════════════════════════
//  setup()
// ═══════════════════════════════════════════════════════════════════════════
void setup() {
  Serial.begin(115200);
  delay(500); // brief settle for USB-CDC

  Serial.println("[healthmon-device] Starting up");

  gLeds.begin();

  // Flash all LEDs briefly to confirm power-on
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

  // ── 1. Update LED animations (runs every tick, non-blocking) ──────────
  gLeds.update();

  // ── 2. Check button ───────────────────────────────────────────────────
  gButton.update();

  // ── 3. Handle pending acknowledge request ─────────────────────────────
  if (gAckPending) {
    gAckPending = false;
    Serial.println("[button] Acknowledge all emails requested");
    acknowledgeAllEmails();
    // Force an immediate re-poll to update LED state
    gLastPollMs = now - Config::POLL_INTERVAL_MS;
  }

  // ── 4. Reconnect Wi-Fi if lost ────────────────────────────────────────
  if (WiFi.status() != WL_CONNECTED) {
    Serial.println("[wifi] Connection lost – reconnecting");
    gState.backendReachable = false;
    applyLeds(gState, gLeds);
    connectWifi();
    return;
  }

  // ── 5. Poll backend on schedule ───────────────────────────────────────
  if (now - gLastPollMs >= Config::POLL_INTERVAL_MS) {
    gLastPollMs = now;
    pollBackend(gState);
    applyLeds(gState, gLeds);
  }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Wi-Fi
// ═══════════════════════════════════════════════════════════════════════════

/** Block until Wi-Fi is connected. Shows a pulsing LED_OK while waiting. */
void connectWifi() {
  Serial.printf("[wifi] Connecting to %s\n", Config::WIFI_SSID);
  WiFi.mode(WIFI_STA);
  WiFi.begin(Config::WIFI_SSID, Config::WIFI_PASSWORD);

  // Pulse LED_OK while waiting (LED_ISSUES off)
  gLeds.setMode(Pins::LED_OK,       LedController::Mode::PULSING);
  gLeds.setMode(Pins::LED_ISSUES,   LedController::Mode::OFF);
  gLeds.setMode(Pins::LED_EMAIL_OK, LedController::Mode::OFF);
  gLeds.setMode(Pins::LED_EMAILS,   LedController::Mode::OFF);

  uint32_t timeout = millis() + 20000; // 20 s max
  while (WiFi.status() != WL_CONNECTED && millis() < timeout) {
    gLeds.update();
    delay(10); // minimal yield; acceptable during blocking connect
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
  // Base64-encode "user:password"
  String creds   = String(Config::BASIC_AUTH);
  // The ESP32 Arduino core provides base64 encoding via mbedtls
  size_t  inLen  = creds.length();
  size_t  outLen = ((inLen + 2) / 3) * 4 + 1;
  char*   buf    = new char[outLen];
  unsigned char elen;
  // Use the mbedtls base64 encoder available in the ESP32 SDK
  mbedtls_base64_encode(
    (unsigned char*)buf, outLen, (size_t*)&elen,
    (const unsigned char*)creds.c_str(), inLen
  );
  buf[elen] = '\0';
  String result = "Basic " + String(buf);
  delete[] buf;
  return result;
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
 * Sets state.allChecksHealthy = true only if ALL entries are healthy.
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

  // Parse JSON array: [{"healthy": bool, ...}, ...]
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
 * - emailSubsystemOn = true if the endpoint responds (200 or empty array)
 * - hasNewEmails     = true if the returned array is non-empty
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

  state.emailSubsystemOn = true;
  state.hasNewEmails     = (doc.as<JsonArray>().size() > 0);
  return true;
}

/**
 * POST /emails/acknowledge-all
 * Fires on button press. Logs the result; does not block the main loop
 * beyond the HTTP round trip.
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
 * Translate AppState into LED modes:
 *
 * LED_OK (green/blue)
 *   ON      if backendReachable && allChecksHealthy && Wi-Fi connected
 *   PULSING otherwise (any problem)
 *
 * LED_ISSUES (red)
 *   PULSING if !backendReachable || !allChecksHealthy
 *   OFF     otherwise
 *
 * LED_EMAIL_OK (green/blue)
 *   ON  if emailSubsystemOn
 *   OFF otherwise
 *
 * LED_EMAILS (yellow)
 *   PULSING if hasNewEmails
 *   OFF     otherwise
 */
void applyLeds(const AppState& state, LedController& leds) {
  bool wifiOk    = (WiFi.status() == WL_CONNECTED);
  bool everythingOk = wifiOk && state.backendReachable && state.allChecksHealthy;

  leds.setMode(Pins::LED_OK,
    everythingOk ? LedController::Mode::ON : LedController::Mode::PULSING);

  leds.setMode(Pins::LED_ISSUES,
    everythingOk ? LedController::Mode::OFF : LedController::Mode::PULSING);

  leds.setMode(Pins::LED_EMAIL_OK,
    state.emailSubsystemOn ? LedController::Mode::ON : LedController::Mode::OFF);

  leds.setMode(Pins::LED_EMAILS,
    state.hasNewEmails ? LedController::Mode::PULSING : LedController::Mode::OFF);
}

// ═══════════════════════════════════════════════════════════════════════════
//  Self-test
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Flash all LEDs in sequence on startup to confirm wiring and PWM work.
 * Uses delay() once — only during setup, not the main loop.
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
  // Brief full-on for all
  for (uint8_t pin : pins) ledcWrite(pin, Config::PWM_MAX);
  delay(400);
  for (uint8_t pin : pins) ledcWrite(pin, 0);
  delay(200);
}
