use std::fmt;
use worker::{Env, Error, Result, console_log};

use crate::errors::AppError;

pub const BASE_URL: &str = "https://www.aiub.edu";
pub const BASE_HOST: &str = "www.aiub.edu";
pub const NOTICE_PATH: &str = "/category/notices";

// Strictly monitor the latest 50 notices on page 1.
pub const LIST_PAGE_SIZE: usize = 50;
pub const MAX_DETAIL_FETCHES_PER_RUN: usize = 5;
pub const MAX_TELEGRAM_SENDS_PER_RUN: usize = 10;
pub const DETAIL_RETRY_COOLDOWN_MINUTES: i64 = 30;
pub const D1_BIND_CHUNK_SIZE: usize = 50;

pub const HTTP_TIMEOUT_MS: u64 = 15_000;
pub const MAX_REDIRECTS: usize = 5;
pub const MAX_URL_LENGTH: usize = 2048;
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
pub const MIN_LISTING_DATE_COVERAGE: f64 = 0.8;
pub const MIN_LISTING_DESC_COVERAGE: f64 = 0.8;
pub const DETAIL_FAILURE_MIN_SAMPLE: usize = 4;
pub const DETAIL_FAILURE_RATIO: f64 = 0.8;
// Debounce for the detail-failure admin alert (M5): without one, a
// sustained AIUB outage alerted on every 5-minute run.
pub const DETAIL_FAILURE_LAST_ALERT_KEY: &str = "detail_failure_last_alert_at";
pub const DETAIL_FAILURE_ALERT_DEBOUNCE_HOURS: i64 = 6;
pub const AIUB_OUTAGE_ALERT_THRESHOLD_MINS: i64 = 60;
pub const AIUB_OUTAGE_DOWN_SINCE_KEY: &str = "aiub_down_since";
pub const AIUB_OUTAGE_ALERTED_KEY: &str = "aiub_outage_alerted";
// Cold-start seed guard (audit #1): set when a seed batch partially
// failed, so the next run re-seeds silently instead of flooding the
// unseeded rows as new notices. Cleared once a seed fully succeeds.
pub const COLD_START_SEED_INCOMPLETE_KEY: &str = "cold_start_seed_incomplete";
// Debounce for the "0 notices parsed" admin alert (M2): a DOM break must not
// alert on every 5-minute run.
pub const EMPTY_LISTING_ALERT_KEY: &str = "empty_listing_last_alert_at";
pub const EMPTY_LISTING_ALERT_DEBOUNCE_HOURS: i64 = 6;

pub const UNIFIED_MAX_MESSAGE_LEN: usize = 4096;
pub const TELEGRAM_PACE_MS: u64 = 1_000;
pub const TELEGRAM_MAX_RETRIES: usize = 2;
pub const TELEGRAM_MAX_RETRY_WAIT_SECS: u64 = 10;
// Auth pause (tg_auth_paused): mirrors the Facebook fb_auth_paused
// pattern — a 401/403 from the Bot API pauses Telegram dispatch for at
// most 24h, so a revoked token cannot burn retries on every run, and a
// transient blip cannot pause posting forever.
pub const TELEGRAM_AUTH_PAUSE_TTL_HOURS: i64 = 24;
pub const TELEGRAM_AUTH_PAUSED_KEY: &str = "tg_auth_paused";
// Debounce for the Telegram auth-failure admin alert.
pub const TELEGRAM_AUTH_LAST_ALERT_KEY: &str = "tg_auth_last_alert_at";
pub const TELEGRAM_AUTH_ALERT_DEBOUNCE_HOURS: i64 = 6;

// Run-level deadline (Medium #5): a single scheduled run must never run
// unboundedly — per-fetch 15s caps alone still allow a pathological run
// (50 detail fetches x retries, slow D1, dispatch pacing) to exceed the
// Worker's CPU/time budget. `run()` selects against this deadline.
pub const RUN_DEADLINE_SECS: u64 = 240;
// Audit #2: dispatch must only start when at least this much time
// remains before the run-level deadline — the deadline drops the run
// wherever it is, including mid-flush, so entering the flush window
// without enough time left risks half-applied marks.
pub const DISPATCH_DEADLINE_BUFFER_SECS: u64 = 30;

// Facebook Graph API configuration
pub const FACEBOOK_GRAPH_API_VERSION: &str = "v27.0";
pub const MAX_FACEBOOK_SENDS_PER_RUN: usize = 5;
pub const FACEBOOK_PACE_MS: u64 = 1_500;
pub const FACEBOOK_BUC_USAGE_THRESHOLD_PCT: u8 = 80;
pub const FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS: i64 = 60;
pub const FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS: i64 = 12;
pub const FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS: i64 = 6;
// Auth pause (fb_auth_paused) must never last forever (H2): a pause older
// than this is treated as expired and cleared, so a transient auth blip
// cannot permanently kill Facebook posting. A genuine auth failure will
// simply re-pause on the next failed post attempt.
pub const FACEBOOK_AUTH_PAUSE_TTL_HOURS: i64 = 24;
pub const FACEBOOK_AUTH_PAUSED_KEY: &str = "fb_auth_paused";
pub const FACEBOOK_COOLDOWN_UNTIL_KEY: &str = "fb_cooldown_until";
// Cap on Meta's estimated_time_to_regain_access (Medium #6): the header
// value is untrusted input, and an unbounded value would push the
// cooldown `Duration::minutes()` in lib.rs arbitrarily far into the
// future. 7 days is the longest a rate-limit cooldown may last.
pub const FACEBOOK_BUC_MAX_COOLDOWN_MINS: i64 = 60 * 24 * 7;

// Medium #9: no derived `Debug` — a derived impl would print the page
// access token into logs/panics. `Secrets` already omits `Debug` for
// the same reason. A hand-written impl redacts the token (the tests
// need `Debug` for `assert_eq!` on `Option<FacebookSecrets>`).
#[derive(Clone, PartialEq, Eq)]
pub struct FacebookSecrets {
    pub page_id: String,
    pub page_access_token: String,
}

impl fmt::Debug for FacebookSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FacebookSecrets")
            .field("page_id", &self.page_id)
            .field("page_access_token", &"[REDACTED]")
            .finish()
    }
}

impl FacebookSecrets {
    #[must_use]
    pub fn from_raw(page_id: Option<&str>, page_access_token: Option<&str>) -> Option<Self> {
        let pid = page_id?.trim();
        let tok = page_access_token?.trim();
        if pid.is_empty() || tok.is_empty() {
            return None;
        }
        Some(Self {
            page_id: pid.to_string(),
            page_access_token: tok.to_string(),
        })
    }
}

#[derive(Clone)]
pub struct Secrets {
    pub telegram_bot_api_key: String,
    pub telegram_chat_id: String,
    pub telegram_admin_chat_id: Option<String>,
    pub facebook: Option<FacebookSecrets>,
}

impl Secrets {
    pub fn from_env(env: &Env) -> Result<Self> {
        // Medium #3: keep the source error — the old code discarded it
        // with `|_|`, so a missing secret and a failing secret store
        // were indistinguishable in the logs.
        let telegram_bot_api_key = env
            .secret("TELEGRAM_BOT_API_KEY")
            .map_err(|e| AppError::Config(format!("missing TELEGRAM_BOT_API_KEY secret: {e}")))?
            .to_string();

        let telegram_chat_id = env
            .secret("TELEGRAM_CHAT_ID")
            .map_err(|e| AppError::Config(format!("missing TELEGRAM_CHAT_ID secret: {e}")))?
            .to_string();

        if telegram_bot_api_key.trim().is_empty() || telegram_chat_id.trim().is_empty() {
            return Err(Error::RustError(
                "TELEGRAM_BOT_API_KEY and TELEGRAM_CHAT_ID must not be empty".into(),
            ));
        }

        let telegram_admin_chat_id = env
            .secret("TELEGRAM_ADMIN_CHAT_ID")
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty());

        let fb_page_id = env.secret("FACEBOOK_PAGE_ID").ok().map(|s| s.to_string());
        let fb_token = env
            .secret("FACEBOOK_PAGE_ACCESS_TOKEN")
            .ok()
            .map(|s| s.to_string());

        let facebook = FacebookSecrets::from_raw(fb_page_id.as_deref(), fb_token.as_deref());
        if facebook.is_none() {
            console_log!(
                "Facebook secrets (FACEBOOK_PAGE_ID / FACEBOOK_PAGE_ACCESS_TOKEN) not configured; skipping Facebook dispatch."
            );
        }

        Ok(Self {
            telegram_bot_api_key,
            telegram_chat_id,
            telegram_admin_chat_id,
            facebook,
        })
    }
}
