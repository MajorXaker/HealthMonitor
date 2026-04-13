# healthmon

A two-part system for monitoring application health and email inboxes:

- **Backend** (`/`) — a lightweight async Rust service that runs healthchecks, polls IMAP accounts, persists state to PostgreSQL, and exposes results over a secured REST API.
- **Hardware device** (`/device`) — a USB-powered indicator based on the Seeed Studio XIAO ESP32-C6, with four LEDs and a button, that polls the backend over Wi-Fi and gives at-a-glance system status.

---

## Repository layout

```
healthmon/
├── src/                   Rust backend source
├── migrations/            SQLx database migrations
├── Dockerfile             Backend container image
├── config.example.json    Auto-generated config template
├── README.md              This file
└── device/
    ├── healthmon_device.ino   Arduino sketch for XIAO ESP32-C6
    └── wiring_instructions.docx  Wiring diagram and setup guide
```

---

## Features

### Backend
- **HTTP healthchecks** — periodically GET an endpoint; marks unhealthy after N consecutive failures
- **File-based healthchecks** — watches a folder for a trigger file; deletes it when found (app re-creates it to signal liveness)
- **Multiple email accounts** — polls any number of IMAP accounts; stores metadata (subject, sender, date) in PostgreSQL
- **Email acknowledgement** — mark individual emails or all emails as received via REST
- **HTTP Basic Auth** — all REST endpoints are protected
- **OpenAPI docs** — live Swagger UI at `/docs`
- **Liveness probe** — `GET /__heartbeat__` requires no auth; returns `{"status":"ok"}`
- **SQLx migrations** — versioned up/down SQL migrations with ordering validation
- **Docker support** — config is mounted at runtime; no image rebuild needed on config change

### Hardware device
- **Wi-Fi connected** — polls the healthmon backend on a configurable interval
- **4 LEDs** — reflect backend reachability, healthcheck status, email subsystem state, and new email presence
- **1 button** — acknowledges all new emails with a single press
- **PWM pulsing** — smooth sinusoidal pulse via ESP32 LEDC hardware; no blocking delays
- **Startup self-test** — flashes all LEDs in sequence on every power-on to confirm wiring

---

## Quick start

### Prerequisites
- Rust 1.85+ (`rustup update stable`)
- PostgreSQL instance
- `config.json` (see [Configuration](#configuration))

### Build & run
```bash
cargo build --release
cp config.example.json config.json   # then edit config.json
./target/release/healthmon
```

### Docker
```bash
docker build -t healthmon .
docker run -d \
  -p 8080:8080 \
  -v $(pwd)/config.json:/app/config.json \
  --name healthmon \
  healthmon
```

Config changes only need a container restart (`docker restart healthmon`) — no rebuild.

---

## Configuration

On every launch the binary writes `config.example.json` with all available fields.
Copy it to `config.json` and fill in real values.

```json
{
  "auth": {
    "username": "admin",
    "password": "changeme"
  },
  "database_url": "postgres://user:pass@localhost:5432/monitor",
  "server": {
    "host": "0.0.0.0",
    "port": 8080, 
    "enable_docs": true
  },
  "healthchecks": [
    {
      "name": "my-api",
      "check_type": "http",
      "address": "http://localhost:3000/health",
      "period_seconds": 30,
      "failure_threshold": 3
    },
    {
      "name": "my-worker",
      "check_type": "file",
      "address": "/var/run/myapp/heartbeat",
      "period_seconds": 60,
      "failure_threshold": 2
    }
  ],
  "emails": [
    {
      "name": "work-inbox",
      "host": "imap.example.com",
      "port": 993,
      "username": "user@example.com",
      "password": "secret",
      "mailbox": "INBOX",
      "poll_interval_seconds": 60,
      "use_tls": true,
      "recent_lookback": "1d"
    }
  ]
}
```

| Field | Description |
|---|---|
| `auth` | Credentials for HTTP Basic Auth on all protected endpoints |
| `database_url` | PostgreSQL connection string |
| `server.host` | Bind address (`0.0.0.0` for all interfaces) |
| `server.port` | HTTP listen port (default `8080`) |
| `server.enable_docs` | Enable OpenAPI docs at `/docs` (default `true`) |
| `healthchecks` | Array of healthcheck definitions |
| `emails` | Array of IMAP account definitions (empty array disables email monitoring) |

### Healthcheck fields
| Field | Type | Description |
|---|---|---|
| `name` | string | Unique identifier shown in API |
| `check_type` | `"http"` \| `"file"` | Type of check |
| `address` | string | URL (http) or folder path (file) |
| `period_seconds` | int | Polling interval |
| `failure_threshold` | int | Consecutive failures before marking unhealthy |

---

## REST API

All endpoints (except `/__heartbeat__`) require `Authorization: Basic <base64(user:pass)>`.

| Method | Path | Description |
|---|---|---|
| `GET` | `/__heartbeat__` | Liveness probe — no auth required |
| `GET` | `/healthchecks` | Current status of all configured checks |
| `GET` | `/emails` | New (unacknowledged) emails from all accounts |
| `POST` | `/emails/acknowledge` | Mark specific emails as received `{"ids":[1,2]}` |
| `POST` | `/emails/acknowledge-all` | Mark all new emails as received |
| `GET` | `/docs` | Swagger UI — interactive OpenAPI documentation |

### Example: check healthchecks
```bash
curl -u admin:changeme http://localhost:8080/healthchecks
```

### Example: acknowledge emails
```bash
curl -u admin:changeme -X POST http://localhost:8080/emails/acknowledge \
  -H 'Content-Type: application/json' \
  -d '{"ids": [1, 2, 3]}'
```

---

## Database migrations

Migrations live in the `migrations/` directory as numbered SQL files:

```
migrations/
  0001_create_emails.up.sql
  0001_create_emails.down.sql
```

SQLx tracks applied migrations in the `_sqlx_migrations` table. On startup:
- Already-applied migrations are skipped
- Pending migrations are applied in order
- A gap or out-of-order migration raises a fatal error

To add a new migration, create `migrations/0002_<name>.up.sql` and `.down.sql`.

---

## Hardware device

The `device/` folder contains the Arduino sketch for a physical status indicator.
See `device/wiring_instructions.docx` for the full wiring diagram and setup guide.

### Hardware required
- Seeed Studio XIAO ESP32-C6
- 4 LEDs: green/blue (×2), red (×1), yellow/amber (×1)
- 4 × 220 Ω resistors
- 1 momentary push button
- Breadboard, jumper wires, USB-C cable

### Pin assignments

| XIAO Pin | Connected to |
|----------|---|
| D10      | Button (other leg to GND) |
| D0       | LED: All OK — green/blue |
| D1       | LED: Healthcheck Issues — red |
| D2       | LED: Email Subsystem Online — green/blue |
| D3       | LED: New Emails — yellow/amber |

Each LED connects through a 220 Ω resistor to GND. The button uses the internal pull-up — no external resistor needed.

### LED behaviour

| LED | Colour | State | Meaning |
|---|---|---|---|
| All OK | Green/blue | Solid | Wi-Fi up, backend reachable, all checks passing |
| All OK | Green/blue | Pulsing | Any problem detected |
| Issues | Red | Pulsing | Backend unreachable or a healthcheck failing |
| Issues | Red | Off | Everything healthy |
| Email OK | Green/blue | Solid | Email subsystem configured and polling |
| Email OK | Green/blue | Off | No email accounts configured |
| New Emails | Yellow | Pulsing | Unacknowledged emails present |
| New Emails | Yellow | Off | No new emails |

### Flashing the device

1. Install [Arduino IDE 2.x](https://www.arduino.cc/en/software)
2. Add the ESP32 boards URL in Preferences:
   ```
   https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json
   ```
3. Install **esp32 by Espressif Systems** ≥ 3.x via Boards Manager
4. Select board: **XIAO_ESP32C6**
5. Install **ArduinoJson** by Benoit Blanchon ≥ 7.x via Library Manager
6. Open `device/healthmon_device.ino`, edit the `Config` namespace:
   ```cpp
   WIFI_SSID      = "your_network"
   WIFI_PASSWORD  = "your_password"
   BACKEND_HOST   = "http://192.168.1.x:8080"
   BASIC_AUTH     = "admin:yourpassword"
   ```
7. Connect the board via USB-C and click Upload

All settings are baked into the firmware at flash time. Any configuration change requires a reflash.

---

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `RUST_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |

---

## Running tests

```bash
cargo test
```

Unit tests cover: config parsing, Basic Auth decoding, healthcheck failure threshold logic, file check behaviour, and email type constraints.

Integration tests (requiring a live database) can be added with `#[ignore]` and run via:
```bash
cargo test -- --include-ignored
```
