<div align="center">

# 🦀 AIUB Notice Checker

**High-performance, edge-native notice monitor and Telegram dispatcher for American International University-Bangladesh (AIUB).**

[![Build & Test](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/test.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/test.yml)
[![Deploy Worker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/deploy.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/deploy.yml)
[![Rust Edition](https://img.shields.io/badge/rust-2021-orange.svg?logo=rust)](https://www.rust-lang.org/)
[![Cloudflare Workers](https://img.shields.io/badge/Cloudflare-Workers-F38020?logo=cloudflare&logoColor=white)](https://workers.cloudflare.com/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Telegram Channel](https://img.shields.io/badge/Telegram-@aiubnotice-2CA5E0?logo=telegram&logoColor=white)](https://t.me/aiubnotice)
[![Facebook Page](https://img.shields.io/badge/Facebook-aiubnotice-1877F2?logo=facebook&logoColor=white)](https://www.facebook.com/aiubnotice)

[Overview](#-overview) •
[Features](#-features) •
[Architecture](#-architecture) •
[Tech Stack](#-tech-stack) •
[Getting Started](#-getting-started) •
[Configuration](#-configuration) •
[Deployment](#-deployment) •
[Observability](#-observability--resilience) •
[Community](#-community--channels) •
[Contributing](#-contributing)

</div>

---

> [!NOTE]
> **Architectural Migration:** This repository was originally a scheduled Python script executed via GitHub Actions. It has been completely rewritten from scratch as a Rust-powered Cloudflare Worker compiled to WebAssembly with Cloudflare D1 storage. The legacy Python implementation is preserved in the [`Deprecated-Python`](https://github.com/origamiofficial/aiub-notice-checker/tree/Deprecated-Python) branch.

---

## 📖 Overview

**AIUB Notice Checker** autonomously monitors the [AIUB Notices Portal](https://www.aiub.edu/category/notices), parses publication feeds, tracks content modifications, and broadcasts instant, cleanly formatted alerts to students and faculty via Telegram.

By running as a serverless Rust Worker on Cloudflare's global edge network, the system eliminates the scheduling jitter, cold starts, and runner spin-up latencies inherent to standard CI-based cron jobs. Every cycle executes in milliseconds with deterministic state tracking backed by Cloudflare D1.

---

## ✨ Features

- ⚡ **Zero-Overhead Edge Execution:** Compiled to `wasm32-unknown-unknown` via Rust for sub-millisecond execution times and minimal memory footprint.
- ⏰ **Deterministic Cron Scheduling:** Cloudflare Worker cron triggers invoke the pipeline every 5 minutes (`*/5 * * * *`) with strict consistency.
- 🔍 **Intelligent Content Hashing:** Uses cryptographic SHA-256 signatures across titles, descriptions, bodies, and attachment sets to detect both newly issued notices and updates to existing entries.
- 🛡️ **Edge Persistence (Cloudflare D1):** Leverages serverless SQLite with primary session consistency (`first-primary`) to avoid read-after-write anomalies across edge regions.
- 🚦 **Rate-Limited Telegram Pacing:** Dispatches announcements sequentially with pacing delays and exponential retry mechanisms to strictly comply with Telegram Bot API limits.
- 🩹 **Silent Mojibake & Encoding Self-Healing:** Seamlessly detects and repairs legacy UTF-8/Latin-1 double-encoded characters without triggering duplicate "Notice Edited" notifications.
- 🚨 **DOM Drift & Health Safeguards:** Employs sample validation thresholds; if the university site changes its layout or drops parsing coverage, executions pause safely and alert administrators.
- ⏱️ **CPU Limit Awareness:** Continuously checks execution budget via `is_near_cpu_limit()` to gracefully defer pending tasks to subsequent runs before edge quotas expire.

---

## 🏗️ Architecture

```mermaid
flowchart TD
    subgraph Schedule ["⏰ Edge Trigger"]
        Cron["Cloudflare Cron Trigger<br/><code>*/5 * * * *</code>"]
    end

    subgraph Worker ["🦀 Rust Cloudflare Worker (WASM)"]
        Scraper["HTML Scraper & Parser<br/>(<code>tl</code> DOM Engine)"]
        Reconciler["State Reconciler &<br/>SHA-256 Hash Differ"]
        RateLimiter["Telegram Pacer &<br/>Circuit Breaker"]
    end

    subgraph External ["🌐 External Targets"]
        AIUB["AIUB Notices Portal<br/><code>aiub.edu/category/notices</code>"]
        TelegramAPI["Telegram Bot API<br/><code>api.telegram.org</code>"]
    end

    subgraph Storage ["💾 Cloudflare D1 (Edge SQLite)"]
        D1[("Session-Consistent DB<br/><code>notices</code> & <code>app_state</code>")]
    end

    subgraph Delivery ["📢 Channels"]
        Subscribers["Telegram Channel<br/>(<code>@aiubnotice</code>)"]
        Admin["Admin Chat<br/>(DOM Drift / Health Alerts)"]
    end

    Cron -->|Triggers every 5 min| Scraper
    Scraper -->|Fetches latest notices| AIUB
    Scraper -->|Extracts metadata & links| Reconciler
    Reconciler <-->|Reads & Upserts (first-primary session)| D1
    Reconciler -->|Queues new & edited notices| RateLimiter
    RateLimiter -->|Dispatches with rate limits & retries| TelegramAPI
    TelegramAPI -->|Broadcasts notice alerts| Subscribers
    RateLimiter -.->|Sends parser/circuit break alerts| Admin
```

---

## 🛠️ Tech Stack

| Layer | Technology | Description |
| :--- | :--- | :--- |
| **Language** | [Rust](https://www.rust-lang.org/) (Edition 2021) | Core application logic, type safety, and memory management |
| **Target Runtime** | [Cloudflare Workers](https://workers.cloudflare.com/) (`wasm32`) | Serverless WebAssembly runtime executing on Cloudflare edge |
| **Database** | [Cloudflare D1](https://developers.cloudflare.com/d1/) | Serverless edge SQLite database with session consistency |
| **Parser** | [`tl`](https://crates.io/crates/tl) | High-performance, zero-allocation native HTML/DOM parser |
| **Hashing** | [`sha2`](https://crates.io/crates/sha2) & [`hex`](https://crates.io/crates/hex) | Cryptographic SHA-256 state differencing |
| **Build & Deploy** | [`wrangler`](https://developers.cloudflare.com/workers/wrangler/) & [`worker-build`](https://crates.io/crates/worker-build) | Cloudflare developer tooling with panic unwinding support |
| **CI / CD** | GitHub Actions | Automated formatting, Clippy lints, unit testing, and continuous deployment |

---

## 📋 Prerequisites

Ensure the following tools are installed locally:

- **[Rust Toolchain](https://rustup.rs/):**
  - Stable toolchain
  - Nightly toolchain (required for `worker-build --panic-unwind`)
  - Target: `wasm32-unknown-unknown`
- **[Node.js](https://nodejs.org/):** Version 20 LTS or 22 LTS
- **[Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/):** Version 3.x or higher (`npm install -g wrangler`)
- **[worker-build](https://github.com/cloudflare/workers-rs):** `cargo install worker-build`
- **Telegram Bot:** Bot Token from [@BotFather](https://t.me/BotFather) and access to channel `@aiubnotice`

---

## ⚙️ Configuration

### Environment Variables & Secrets

All secrets are managed via Cloudflare Workers Secrets and injected at runtime:

| Variable Name | Type | Required | Description |
| :--- | :---: | :---: | :--- |
| `TELEGRAM_BOT_API_KEY` | Secret | **Yes** | Telegram Bot API authorization token generated by [@BotFather](https://t.me/BotFather). |
| `TELEGRAM_CHAT_ID` | Secret | **Yes** | Target channel username (`@aiubnotice`) or numerical chat ID. |
| `TELEGRAM_ADMIN_CHAT_ID` | Secret | *Optional* | Telegram numerical user ID to receive critical operational alerts (e.g. DOM drift). |

### Cloudflare Worker Configuration (`wrangler.toml`)

The Worker relies on `wrangler.toml` for binding definition and scheduling:

```toml
name = "aiub-notice-checker"
main = "build/index.js"
compatibility_date = "2026-11-13"
workers_dev = false
preview_urls = false

[build]
command = "worker-build --release --panic-unwind"

[triggers]
crons = ["*/5 * * * *"]

[observability]
enabled = true

[[d1_databases]]
binding = "DB"
database_name = "aiub-notices"
database_id = "eb3f0605-6564-4082-8e47-16b308c500ad"
migrations_dir = "migrations"
```

<details>
<summary><strong>🗄️ Database Schema & Migrations (<code>migrations/0001_init.sql</code>)</strong></summary>

```sql
CREATE TABLE IF NOT EXISTS notices (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    link TEXT NOT NULL,
    published_date TEXT,
    body_text TEXT NOT NULL DEFAULT '',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    content_hash TEXT NOT NULL DEFAULT '',
    first_seen_at TEXT NOT NULL DEFAULT '',
    last_seen_at TEXT NOT NULL DEFAULT '',
    sent_at TEXT,
    last_notified_hash TEXT,
    detail_checked_at TEXT,
    detail_fetched_at TEXT,
    notification_attempted_at TEXT,
    content_changed_at TEXT NOT NULL DEFAULT ''
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_notices_link ON notices(link);
CREATE INDEX IF NOT EXISTS idx_notices_published_date ON notices(published_date);

CREATE TABLE IF NOT EXISTS app_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```
</details>

---

## 🚀 Getting Started (Local Development)

### 1. Clone & Set Up the Repository

```bash
git clone https://github.com/origamiofficial/aiub-notice-checker.git
cd aiub-notice-checker
```

### 2. Install Rust WebAssembly Targets

```bash
# Add WebAssembly compilation targets
rustup target add wasm32-unknown-unknown
rustup toolchain install nightly --target wasm32-unknown-unknown --component rust-src

# Install worker-build
cargo install worker-build
```

### 3. Configure Local Secrets

Create a local `.dev.vars` file in the root directory (never commit this file):

```ini
TELEGRAM_BOT_API_KEY="your-telegram-bot-token-from-botfather"
TELEGRAM_CHAT_ID="@aiubnotice"
TELEGRAM_ADMIN_CHAT_ID="your-admin-chat-id"
```

### 4. Initialize Local D1 Database

Run initial migrations against the local SQLite state:

```bash
npx wrangler d1 migrations apply aiub-notices --local
```

### 5. Start Development Server & Trigger Cron

Start the Wrangler development server with test triggers enabled:

```bash
npx wrangler dev --test-scheduled
```

In another terminal, manually dispatch a scheduled event to verify execution:

```bash
curl "http://localhost:8787/__scheduled?cron=*+*+*+*+*"
```

---

## 🚢 Deployment

### Manual Deployment via Wrangler CLI

1. **Log in to Cloudflare:**
   ```bash
   npx wrangler login
   ```

2. **Apply Remote Migrations:**
   ```bash
   npx wrangler d1 migrations apply aiub-notices --remote
   ```

3. **Upload Remote Secrets:**
   ```bash
   npx wrangler secret put TELEGRAM_BOT_API_KEY
   npx wrangler secret put TELEGRAM_CHAT_ID
   npx wrangler secret put TELEGRAM_ADMIN_CHAT_ID
   ```

4. **Deploy the Worker:**
   ```bash
   npx wrangler deploy
   ```

<details>
<summary><strong>🤖 Automated CI/CD (GitHub Actions)</strong></summary>

The project includes pre-configured workflows under `.github/workflows/deploy.yml` that build and deploy automatically on merges to `main`.

Configure the following secrets in **GitHub Repository Settings ➔ Secrets and variables ➔ Actions**:
- `CLOUDFLARE_API_TOKEN`: Cloudflare API token with `Workers:Edit` and `D1:Edit` permissions.
- `CLOUDFLARE_ACCOUNT_ID`: Your Cloudflare Account ID.

All other runtime secrets (`TELEGRAM_*`) reside directly in Cloudflare Secrets and remain persistent across deployments.
</details>

---

## 💬 Message Format & Sample Output

When a new notice is detected, the bot dispatches an alert with markdown formatting and direct links:

```text
Final Exam Routine Fall 2026-2027

Date: 15 Nov 2026

The students of the Undergraduate and Graduate programs are hereby informed that the Final Examinations of Fall 2026-2027 will commence from...

https://www.aiub.edu/category/notices/final-exam-routine-fall-2026-2027
```

If an existing notice is updated on the university site (such as modified exam dates or altered PDF attachments), an `[EDITED]` tag is prepended:

```text
[EDITED] Revised Final Exam Routine Fall 2026-2027

Date: 16 Nov 2026

Notice update: The exam schedule for CSE 4101 has been rescheduled to Room 3201...

https://www.aiub.edu/category/notices/final-exam-routine-fall-2026-2027
```

---

## 🩺 Observability & Resilience

- **Distributed Traces:** Employs Cloudflare Workers Tracing with named span instrumentation (`fetch_listing`, `load_existing_from_d1`, `fetch_detail_pages`, `d1_batch_upsert`, `dispatch_telegram_notifications`).
- **Circuit Breaker:** If sampled detail pages fail parsing validation at a rate $\ge 80\%$, the Worker aborts state modification and alerts the admin chat of suspected upstream DOM layout restructuring.
- **Backpressure Protection:** In each 5-minute cycle, detail page fetching is capped at `MAX_DETAIL_FETCHES_PER_RUN = 5` with concurrency set to 3. Telegram transmissions are paced to 1-second intervals with a maximum of 10 messages per batch to avoid downstream rate limits.
- **Log Aggregation:** View live tail logs using Wrangler:
  ```bash
  npx wrangler tail
  ```

---

## 🌐 Community & Channels

Follow the automated feeds directly on social platforms:

- **Telegram Channel:** [@aiubnotice](https://t.me/aiubnotice) (Instant push notifications)
- **Facebook Page:** [AIUB Notice Community](https://www.facebook.com/aiubnotice)

---

## 🧪 Testing & Quality Assurance

Run the complete offline test and linting suite locally:

```bash
# Check formatting
cargo fmt --check

# Run Clippy with zero-warning tolerance
cargo clippy --all-targets --all-features -- -D warnings

# Execute unit and integration tests
cargo test --all-features

# Verify WebAssembly compilation target
cargo check --target wasm32-unknown-unknown
```

---

## 📁 Project Structure

```text
aiub-notice-checker/
├── .github/
│   └── workflows/
│       ├── test.yml                   # CI pipeline: fmt, clippy, unit tests, wasm check
│       └── deploy.yml                 # Automated Cloudflare Worker deployment
├── migrations/
│   └── 0001_init.sql                  # Cloudflare D1 initial schema and indexes
├── src/
│   ├── config.rs                      # Constants, limits, tuning thresholds, and secrets
│   ├── db.rs                          # D1 query builders, batch executions, and state setters
│   ├── lib.rs                         # Worker entrypoint, cron dispatcher, and orchestrator
│   ├── models.rs                      # Notice data models, hashing logic, and text formatting
│   ├── scraper.rs                     # HTML fetching, redirect handling, and tl DOM parsing
│   ├── telegram.rs                    # Telegram Bot API client, rate-limit handler, and retries
│   └── tests.rs                       # Comprehensive unit and regression test suite
├── Cargo.toml                         # Rust crate manifest, dependencies, and release profiles
├── LICENSE                            # MIT License
├── README.md                          # Project documentation
└── wrangler.toml                      # Cloudflare Worker configuration, crons, and D1 bindings
```

---

## 🤝 Contributing

Contributions are welcomed! Follow these steps to submit improvements:

1. **Fork the Repository:** Create your own feature branch (`git checkout -b feat/amazing-feature`).
2. **Implement Your Changes:** Follow idiomatic Rust practices and ensure all public functions are documented.
3. **Verify Standards:** Ensure `cargo fmt --check`, `cargo clippy`, and `cargo test` pass without warnings.
4. **Commit Changes:** Write clear, conventional commit messages (`git commit -m 'feat: support multi-page notice scanning'`).
5. **Open a Pull Request:** Submit your PR against `main` detailing the rationale and changes.

---

## 📄 License

This project is open-source software licensed under the **[MIT License](./LICENSE)**.

---

## 🙏 Acknowledgments

- [American International University-Bangladesh (AIUB)](https://www.aiub.edu) for notice information.
- [Cloudflare Workers](https://workers.cloudflare.com/) and [workers-rs](https://github.com/cloudflare/workers-rs) team for the edge WebAssembly ecosystem.
- [tl HTML Parser](https://github.com/y21/tl) for fast HTML parsing in Rust.
