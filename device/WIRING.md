# healthmon Device — Wiring & Setup Guide
**XIAO ESP32-C6 Status Indicator — v1.2**

---

## Overview

This device connects to a healthmon backend over Wi-Fi and shows system health
on four LEDs and a 128x64 OLED display. A rotary encoder switches between
screens, and a button acknowledges emails. It is powered by USB-C and based on
the Seeed Studio XIAO ESP32-C6.

### Hardware required

- Seeed Studio XIAO ESP32-C6 (× 1)
- Green or blue LED — "All OK" indicator (× 1)
- Red LED — "Healthcheck Issues" indicator (× 1)
- Green or blue LED — "Email Subsystem Online" indicator (× 1)
- Yellow or amber LED — "New Emails" indicator (× 1)
- 220 Ω resistors (× 4, one per LED)
- Momentary push button (× 1)
- 128x64 monochrome I2C OLED, SSD1306 or SH1106 controller (× 1)
- Rotary encoder module with pins CLK, DT, SW, +, GND (KY-040 style) (× 1)
- Breadboard and jumper wires
- USB-C cable for power and programming

---

## Pin Assignments

| XIAO Pin | ESP32-C6 GPIO | Connected To         | Notes                                              |
|----------|---------------|----------------------|----------------------------------------------------|
| D0       | GPIO0         | LED_OK anode         | Via 220 Ω resistor to GND — green or blue LED      |
| D1       | GPIO1         | LED_ISSUES anode     | Via 220 Ω resistor to GND — red LED                |
| D2       | GPIO2         | LED_EMAIL_OK anode   | Via 220 Ω resistor to GND — green or blue LED      |
| D3       | GPIO21        | LED_EMAILS anode     | Via 220 Ω resistor to GND — yellow or amber LED    |
| D4       | GPIO22        | Display SDA          | Default I2C data pin                               |
| D5       | GPIO23        | Display SCL          | Default I2C clock pin                              |
| D6       | GPIO16        | — (free)             | UART0 TX; the boot ROM may print on it, keep free  |
| D7       | GPIO17        | Encoder SW           | Wired for future use; ignored by firmware v1.2     |
| D8       | GPIO19        | Encoder CLK          | Internal pull-up in firmware                       |
| D9       | GPIO20        | Encoder DT           | Internal pull-up in firmware                       |
| D10      | GPIO18        | Button (one leg)     | Other leg to GND; internal pull-up in firmware     |
| 3V3      | 3V3           | Display VCC, encoder `+` | **Use 3V3, never 5V** (see below)              |
| GND      | GND           | Common ground        | LEDs, button, display, encoder                     |
| 5V       | VBUS          | USB-C power          | Nothing connected                                  |

None of the XIAO D-pins is an ESP32-C6 boot-mode (strapping) pin, so any of
them is safe at power-on.

### Why 3V3 for the display and the encoder

- The KY-040-style encoder module has 10 kΩ pull-up resistors from CLK/DT (and
  sometimes SW) to its `+` pin. With `+` on 5V, those lines would carry 5V into
  3.3V-only GPIO inputs.
- Most OLED modules have I2C pull-ups to their VCC pin, so the same applies to
  SDA/SCL.

---

## Wiring Diagram

```
XIAO ESP32-C6
┌─────────────────────────┐
│                     5V  │── (not connected)
│                    3V3  │──┬───────────── OLED VCC
│                         │  └───────────── ENCODER +
│                    GND  │──┬──────────────────────────────────────┐
│                         │  ├── OLED GND, ENCODER GND              │
│                         │  │                                      │
│  D4  (GPIO22) ──────────┼──┼── OLED SDA                           │
│  D5  (GPIO23) ──────────┼──┼── OLED SCL                           │
│                         │  │                                      │
│  D7  (GPIO17) ──────────┼──┼── ENCODER SW  (unused for now)       │
│  D8  (GPIO19) ──────────┼──┼── ENCODER CLK                        │
│  D9  (GPIO20) ──────────┼──┼── ENCODER DT                         │
│                         │  │                                      │
│  D10 (GPIO18) ──────────┼──┤ BUTTON ── GND                        │
│                         │  │  (internal pull-up in firmware)      │
│                         │  │                                      │
│  D0  (GPIO0)  ──────────┼──┤ 220Ω ── LED_OK (+) ── GND            │
│  D1  (GPIO1)  ──────────┼──┤ 220Ω ── LED_ISSUES (+) ── GND        │
│  D2  (GPIO2)  ──────────┼──┤ 220Ω ── LED_EMAIL_OK (+) ── GND      │
│  D3  (GPIO21) ──────────┼──┤ 220Ω ── LED_EMAILS (+) ── GND        │
└─────────────────────────┘
```

> The longer leg of each LED is the anode (+) and connects toward the
> microcontroller pin through the resistor. The shorter leg is the
> cathode (–) and connects to GND.

---

## Display

The firmware uses U8g2 in hardware-I2C, full-framebuffer mode. Select the
constructor matching your module near the top of `healthmon_device.ino`:

| Module                          | Constructor                              |
|---------------------------------|------------------------------------------|
| 0.96" (most common), SSD1306    | `U8G2_SSD1306_128X64_NONAME_F_HW_I2C`    |
| 1.3", SH1106                    | `U8G2_SH1106_128X64_NONAME_F_HW_I2C`     |

If the picture is shifted by two pixels with noise along one edge, the module
is an SH1106 — switch the constructor.

### Screens

Rotate the encoder to move between screens; the list wraps around.

| Screen          | Content                                                              |
|-----------------|----------------------------------------------------------------------|
| Email (one each)| Header `Mail n/N` + account, sender, subject (2 lines), local date   |
| Healthchecks    | Header with `ok/total OK`, 4 services per page; failing ones inverted with `FAIL` |
| Rates           | Plain text from the backend (placeholder until backend support)      |

- Emails are listed newest first. Up to 10 are kept on the device; the last
  one shows `+N more` if the backend has more.
- Email dates are shown in local time (`TZ_OFFSET_MINUTES`, default +180).
- The top pixel row is reserved for the hold progress bar.
- The display blanks after 5 minutes without input (`DISPLAY_TIMEOUT_MS`,
  `0` disables). The first turn or press after that only wakes it; a newly
  arrived email also wakes it.

---

## Rotary Encoder

- One click = one screen. If your encoder needs two clicks per screen, set
  `ENCODER_STEPS_PER_DETENT = 2`.
- If clockwise goes to the previous screen, set `ENCODER_REVERSE = true`
  (or swap the CLK and DT wires).
- The push switch (SW) is wired to D7 but not used in v1.2.

---

## Acknowledge Button

| Gesture                         | Action                                                   |
|---------------------------------|----------------------------------------------------------|
| Short press on an email screen  | `POST /emails/acknowledge {"ids":[id]}`; the next screen is shown immediately |
| Short press on another screen   | Nothing                                                  |
| Hold for 5 seconds              | `POST /emails/acknowledge-all`; a bar fills the top pixel row while holding |

- Short-press actions fire on release, so a hold never also acknowledges a
  single email.
- The hold bar only appears when there are emails to acknowledge.
- LED_EMAILS blinks three times as confirmation. If the request fails, the
  email reappears after the next poll.

The button uses the internal pull-up resistor — no external resistor is needed.
Connect one leg to D10 and the other leg directly to GND.

---

## LED Behaviour

| LED          | Colour         | State   | Meaning                                                          |
|--------------|----------------|---------|------------------------------------------------------------------|
| LED_OK       | Green or blue  | Solid   | Wi-Fi connected, backend reachable, all healthchecks passing     |
| LED_OK       | Green or blue  | Pulsing | Any problem: Wi-Fi lost, backend unreachable, or a check failing |
| LED_ISSUES   | Red            | Pulsing | At least one healthcheck failing, backend unreachable, or no Wi-Fi |
| LED_ISSUES   | Red            | Off     | All healthchecks passing and backend reachable                   |
| LED_EMAIL_OK | Green or blue  | Solid   | Email subsystem configured and backend is polling for emails     |
| LED_EMAIL_OK | Green or blue  | Off     | No email accounts configured in healthmon                        |
| LED_EMAILS   | Yellow / amber | Pulsing | New unacknowledged emails are present                            |
| LED_EMAILS   | Yellow / amber | Off     | No new emails                                                    |

---

## Arduino IDE Setup & Flashing

1. Install [Arduino IDE 2.x](https://www.arduino.cc/en/software).

2. Open **File → Preferences → Additional Boards Manager URLs** and add:
   ```
   https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json
   ```

3. Open **Tools → Board → Boards Manager**, search for `esp32`, install
   **esp32 by Espressif Systems** version 3.x or later.

4. Select board: **Tools → Board → ESP32 Arduino → XIAO_ESP32C6**.
   Set **Tools → USB CDC On Boot → Enabled** to see the log over USB-C.

5. Install libraries via **Sketch → Include Library → Manage Libraries**:
   - `ArduinoJson` by Benoit Blanchon, 7.x or later
   - `U8g2` by oliver, 2.34 or later

6. Open `healthmon_device.ino` and edit the `Config` namespace at the top:

   ```cpp
   constexpr const char* WIFI_SSID     = "your_network";
   constexpr const char* WIFI_PASSWORD = "your_password";
   constexpr const char* BACKEND_HOST  = "http://192.168.1.x:8080";
   constexpr const char* BASIC_AUTH    = "admin:yourpassword";
   ```

7. Connect the XIAO ESP32-C6 via USB-C, select the port under **Tools → Port**,
   and click **Upload (→)**.

> All configuration is baked into the firmware at flash time. To change any
> setting, edit the `Config` namespace and reflash the device.

The v1.2 build uses about 90% of the default 1.3 MB app partition. If a future
change does not fit, pick a larger app partition under
**Tools → Partition Scheme** (for example "Huge APP").

---

## Startup

On every power-on or reset, the display shows a boot screen while the LEDs run
a self-test: each LED lights up in sequence (OK → Issues → Email OK →
New Emails), then all four light together for 400 ms. The device then connects
to Wi-Fi and polls the backend; the display shows `Connecting Wi-Fi` /
`Polling backend...` until the first data arrives.

---

## Power Supply

The device is powered entirely via the USB-C connector on the XIAO ESP32-C6.
No separate power supply is required. Any standard USB charger or computer USB
port (5 V, ≥ 500 mA) is sufficient.
