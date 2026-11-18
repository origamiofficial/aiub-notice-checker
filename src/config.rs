use worker::{Env, Error, Result};

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

pub const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;
pub const TELEGRAM_PACE_MS: u64 = 1_000;
pub const TELEGRAM_MAX_RETRIES: usize = 2;
pub const TELEGRAM_MAX_RETRY_WAIT_SECS: u64 = 10;

pub struct Secrets {
    pub telegram_bot_api_key: String,
    pub telegram_chat_id: String,
    pub telegram_admin_chat_id: Option<String>,
}

impl Secrets {
    pub fn from_env(env: &Env) -> Result<Self> {
        let telegram_bot_api_key = env
            .secret("TELEGRAM_BOT_API_KEY")
            .map_err(|_| Error::RustError("Missing TELEGRAM_BOT_API_KEY secret".into()))?
            .to_string();

        let telegram_chat_id = env
            .secret("TELEGRAM_CHAT_ID")
            .map_err(|_| Error::RustError("Missing TELEGRAM_CHAT_ID secret".into()))?
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

        Ok(Self {
            telegram_bot_api_key,
            telegram_chat_id,
            telegram_admin_chat_id,
        })
    }
}
