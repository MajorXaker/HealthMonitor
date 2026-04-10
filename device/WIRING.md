# healthmon Device — Wiring & Setup Guide
**XIAO ESP32-C6 Status Indicator**

---

## Overview

This device connects to a healthmon backend over Wi-Fi and displays system health
status via four LEDs and a button. It is powered by USB-C and based on the
Seeed Studio XIAO ESP32-C6.

### Hardware required

- Seeed Studio XIAO ESP32-C6 (× 1)
- Green or blue LED — "All OK" indicator (× 1)
- Red LED — "Healthcheck Issues" indicator (× 1)
- Green or blue LED — "Email Subsystem Online" indicator (× 1)
- Yellow or amber LED — "New Emails" indicator (× 1)
- 220 Ω resistors (× 4, one per LED)
- Momentary push button (× 1)
- Breadboard and jumper wires
- USB-C cable for power and programming

---

## Pin Assignments

| XIAO Pin | ESP32-C6 GPIO | Connected To         | Notes                                              |
|----------|---------------|----------------------|----------------------------------------------------|
| D1       | GPIO1         | Button (one leg)     | Other leg to GND; internal pull-up in firmware     |
| D2       | GPIO2         | LED_OK anode         | Via 220 Ω resistor to GND — green or blue LED      |
| D3       | GPIO21        | LED_ISSUES anode     | Via 220 Ω resistor to GND — red LED               |
| D9       | GPIO20        | LED_EMAIL_OK anode   | Via 220 Ω resistor to GND — green or blue LED      |
| D10      | GPIO18        | LED_EMAILS anode     | Via 220 Ω resistor to GND — yellow or amber LED    |
| GND      | GND           | Common ground        | Shared by all LED resistors and button             |
| 5V       | VBUS          | USB-C power          | Board powered via USB-C, no external supply needed |

---

## Wiring Diagram

```
XIAO ESP32-C6
┌─────────────────────────┐
│                     5V  │── USB-C power (no connection needed)
│                    GND  │──┬──────────────────────────────────────┐
│                         │  │                                      │
│  D1  (GPIO1)  ──────────┼──┤ BUTTON ── GND                       │
│                         │  │  (internal pull-up in firmware)      │
│                         │  │                                      │
│  D2  (GPIO2)  ──────────┼──┤ 220Ω ── LED_OK (+) ── GND           │
│                         │  │         [green/blue]                 │
│                         │  │                                      │
│  D3  (GPIO21) ──────────┼──┤ 220Ω ── LED_ISSUES (+) ── GND       │
│                         │  │         [red]                        │
│                         │  │                                      │
│  D9  (GPIO20) ──────────┼──┤ 220Ω ── LED_EMAIL_OK (+) ── GND     │
│                         │  │         [green/blue]                 │
│                         │  │                                      │
│  D10 (GPIO18) ──────────┼──┤ 220Ω ── LED_EMAILS (+) ── GND       │
│                         │     [yellow/amber]                      │
└─────────────────────────┘
```

> The longer leg of each LED is the anode (+) and connects toward the
> microcontroller pin through the resistor. The shorter leg is the
> cathode (–) and connects to GND.

---

## LED Behaviour

| LED          | Colour         | State   | Meaning                                                          |
|--------------|----------------|---------|------------------------------------------------------------------|
| LED_OK       | Green or blue  | Solid   | Wi-Fi connected, backend reachable, all healthchecks passing     |
| LED_OK       | Green or blue  | Pulsing | Any problem: Wi-Fi lost, backend unreachable, or a check failing |
| LED_ISSUES   | Red            | Pulsing | At least one healthcheck failing or backend unreachable          |
| LED_ISSUES   | Red            | Off     | All healthchecks passing and backend reachable                   |
| LED_EMAIL_OK | Green or blue  | Solid   | Email subsystem configured and backend is polling for emails     |
| LED_EMAIL_OK | Green or blue  | Off     | No email accounts configured in healthmon                        |
| LED_EMAILS   | Yellow / amber | Pulsing | New unacknowledged emails are present                            |
| LED_EMAILS   | Yellow / amber | Off     | No new emails                                                    |

---

## Acknowledge Button

Pressing the button sends a `POST /emails/acknowledge-all` request to the
healthmon backend, marking all current emails as received. The LED_EMAILS
indicator will stop pulsing once the next poll cycle completes (within the
configured poll interval, default 15 seconds).

The button uses the internal pull-up resistor — no external resistor is needed.
Connect one leg to D1 and the other leg directly to GND.

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

5. Install the ArduinoJson library:
   **Sketch → Include Library → Manage Libraries** → search `ArduinoJson`
   by Benoit Blanchon → install version 7.x or later.

6. Open `healthmon_device.ino` and edit the `Config` namespace at the top:

   ```cpp
   constexpr const char* WIFI_SSID     = "your_network";
   constexpr const char* WIFI_PASSWORD = "your_password";
   constexpr const char* BACKEND_HOST  = "http://192.168.1.x:8080";
   constexpr const char* BASIC_AUTH    = "admin:yourpassword";
   ```

7. Connect the XIAO ESP32-C6 via USB-C.

8. Select the correct port under **Tools → Port**.

9. Click **Upload (→)**. The IDE will compile and flash the firmware.

> All configuration is baked into the firmware at flash time. To change any
> setting (Wi-Fi credentials, backend address, poll interval), edit the
> `Config` namespace and reflash the device.

---

## Startup Self-Test

On every power-on or reset, the device runs a brief self-test: each LED lights
up in sequence (OK → Issues → Email OK → New Emails), then all four illuminate
together for 400 ms. This confirms all LEDs and their resistors are correctly
wired before normal operation begins.

---

## Power Supply

The device is powered entirely via the USB-C connector on the XIAO ESP32-C6.
No separate power supply is required. Any USB-A to USB-C cable and a standard
USB charger or computer USB port (5 V, ≥ 500 mA) is sufficient.
