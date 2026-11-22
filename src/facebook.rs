use chrono::{Duration, Utc};
use futures_util::future::{select, Either};
use serde::Deserialize;
use std::pin::pin;
use std::time::Duration as StdDuration;
use worker::{
    console_log,
    d1::{D1Database, D1DatabaseSession},
    AbortController, Delay, Fetch, Headers, Method, Request, RequestInit, Result,
};

use crate::config::{
    FacebookSecrets, Secrets, FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS, FACEBOOK_AUTH_PAUSED_KEY,
    FACEBOOK_AUTH_PAUSE_TTL_HOURS, FACEBOOK_BUC_USAGE_THRESHOLD_PCT, FACEBOOK_COOLDOWN_UNTIL_KEY,
    FACEBOOK_GRAPH_API_VERSION, FACEBOOK_PACE_MS, FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS,
    HTTP_TIMEOUT_MS,
};
use crate::db;
use crate::telegram;

/// Parsed Business Use Case (BUC) rate-limit usage from Meta's response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BucUsage {
    pub call_count: u8,
    pub total_cputime: u8,
    pub total_time: u8,
    pub estimated_time_to_regain_access: i64, // in minutes
}

impl BucUsage {
    /// Returns true if any rate-limit percentage reaches or exceeds the safe threshold.
    pub fn is_approaching_limit(&self) -> bool {
        self.call_count >= FACEBOOK_BUC_USAGE_THRESHOLD_PCT
            || self.total_cputime >= FACEBOOK_BUC_USAGE_THRESHOLD_PCT
            || self.total_time >= FACEBOOK_BUC_USAGE_THRESHOLD_PCT
    }

    /// Returns the highest single usage percentage across call count, cputime, and total time.
    pub fn max_usage(&self) -> u8 {
        self.call_count.max(self.total_cputime).max(self.total_time)
    }
}

/// Parses Meta's `X-Business-Use-Case-Usage` (or `X-App-Usage`) JSON header value.
///
/// Example `X-Business-Use-Case-Usage` structure:
/// `{"1234567890":[{"type":"pages","call_count":45,"total_cputime":30,"total_time":20,"estimated_time_to_regain_access":0}]}`
pub fn parse_buc_usage_header(header_value: &str) -> Option<BucUsage> {
    let parsed: serde_json::Value = serde_json::from_str(header_value).ok()?;

    // Case 1: X-Business-Use-Case-Usage (map of page/app ID to array of usage entries)
    if let Some(map) = parsed.as_object() {
        for val in map.values() {
            if let Some(arr) = val.as_array() {
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        let call_count = obj
                            .get("call_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0)
                            .min(100) as u8;
                        let total_cputime = obj
                            .get("total_cputime")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0)
                            .min(100) as u8;
                        let total_time = obj
                            .get("total_time")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0)
                            .min(100) as u8;
                        let estimated_time_to_regain_access = obj
                            .get("estimated_time_to_regain_access")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);

                        return Some(BucUsage {
                            call_count,
                            total_cputime,
                            total_time,
                            estimated_time_to_regain_access,
                        });
                    }
                }
            }
        }
    }

    // Case 2: X-App-Usage (flat object: {"call_count": 45, "total_cputime": 30, "total_time": 20})
    if let Some(obj) = parsed.as_object() {
        if obj.contains_key("call_count") {
            let call_count = obj
                .get("call_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(100) as u8;
            let total_cputime = obj
                .get("total_cputime")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(100) as u8;
            let total_time = obj
                .get("total_time")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(100) as u8;

            return Some(BucUsage {
                call_count,
                total_cputime,
                total_time,
                estimated_time_to_regain_access: 0,
            });
        }
    }

    None
}

/// Actionable categorization of Meta Graph API errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaErrorAction {
    /// Rate limit reached (80001, 32, 4, 17, 341, 613). Stop immediately, set cooldown.
    RateLimitCooldown { estimated_minutes: i64 },
    /// Spam or policy violation block (368). Hard stop, set long cooldown, alert admin.
    SpamBlock,
    /// Duplicate consecutive post (506). Mark handled to prevent endless loop.
    DuplicatePost,
    /// Token revoked, expired, password change, or security checkpoint (190, 102). Pause auth, alert admin.
    AuthFailure { code: i64, subcode: Option<i64> },
    /// Permission revoked or missing (10, 200..=299). Pause auth, alert admin.
    PermissionError { code: i64 },
    /// Transient Meta server glitch or network timeout (1, 2, 5xx). Defer or retry once.
    TransientError,
    /// Unclassified error code.
    Unknown { code: i64 },
}

/// Classifies a Meta error code and optional subcode into a deterministic action.
pub fn classify_meta_error(code: i64, subcode: Option<i64>) -> MetaErrorAction {
    match code {
        80001 | 32 | 4 | 17 | 341 | 613 => MetaErrorAction::RateLimitCooldown {
            estimated_minutes: 0,
        },
        368 => MetaErrorAction::SpamBlock,
        506 => MetaErrorAction::DuplicatePost,
        190 | 102 => MetaErrorAction::AuthFailure { code, subcode },
        10 | 200..=299 => MetaErrorAction::PermissionError { code },
        1 | 2 => MetaErrorAction::TransientError,
        _ => MetaErrorAction::Unknown { code },
    }
}

/// Serialises an auth-pause value for `app_state.fb_auth_paused` as
/// `<RFC3339 timestamp>|<message>`, so the pause can expire (H2).
pub fn format_auth_pause_value(message: &str) -> String {
    format!("{}|{}", Utc::now().to_rfc3339(), message)
}

/// Parses a value produced by [`format_auth_pause_value`].
/// Returns `(paused_at, message)`, or `None` for a legacy value that was
/// stored before timestamps were added (a bare message with no `|`).
pub fn parse_auth_pause_value(value: &str) -> Option<(chrono::DateTime<Utc>, String)> {
    let (ts, message) = value.split_once('|')?;
    let parsed = chrono::DateTime::parse_from_rfc3339(ts)
        .ok()?
        .with_timezone(&Utc);
    Some((parsed, message.to_string()))
}

/// Result of attempting to publish a post to Facebook.
#[derive(Debug)]
pub enum FacebookPostResult {
    /// Successfully published. Contains Meta's post ID (e.g. `{page_id}_{post_id}`) and parsed usage.
    Success {
        post_id: String,
        usage: Option<BucUsage>,
    },
    /// Rate limited (error 80001, 32, etc.). Contains cooldown in minutes.
    RateLimited { cooldown_minutes: i64 },
    /// Blocked for spam/policy (error 368).
    SpamBlocked,
    /// Duplicate text rejected by Meta (error 506).
    Duplicate,
    /// Token or permission error (190, 102, 10, 2xx). Needs admin intervention.
    AuthError { message: String },
    /// Transient Meta 5xx or timeout.
    TransientFailure { message: String },
    /// Other unclassified error.
    UnknownError { message: String },
}

#[derive(Deserialize)]
struct GraphPostSuccessResponse {
    id: Option<String>,
}

#[derive(Deserialize)]
struct GraphErrorContainer {
    error: Option<GraphErrorDetails>,
}

#[derive(Deserialize)]
struct GraphErrorDetails {
    message: Option<String>,
    code: Option<i64>,
    error_subcode: Option<i64>,
}

/// Publishes a plain-text status update to `POST https://graph.facebook.com/v27.0/{page-id}/feed`.
///
/// Security rules:
/// - Strictly passes the token via `Authorization: Bearer <token>` HTTP header, NEVER in query params.
/// - Strictly sends only `{"message": message}` in the POST body; completely omits `link` and `picture`.
pub async fn publish_post(
    fb_secrets: &FacebookSecrets,
    message: &str,
) -> Result<FacebookPostResult> {
    let url = format!(
        "https://graph.facebook.com/{}/{}/feed",
        FACEBOOK_GRAPH_API_VERSION, fb_secrets.page_id
    );

    let body_json = serde_json::json!({
        "message": message
    })
    .to_string();

    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    // Token strictly in Authorization header — never exposes token in URLs, traces, or logs
    headers.set(
        "Authorization",
        &format!("Bearer {}", fb_secrets.page_access_token),
    )?;

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(body_json.into()));

    let req = Request::new_with_init(&url, &init)?;
    let controller = AbortController::default();
    let signal = controller.signal();

    let fetch = Fetch::Request(req);
    let send_fut = fetch.send_with_signal(&signal);
    let timeout_fut = Delay::from(StdDuration::from_millis(HTTP_TIMEOUT_MS));

    let mut resp = match select(pin!(send_fut), pin!(timeout_fut)).await {
        Either::Left((res, _)) => res?,
        Either::Right(((), _)) => {
            controller.abort();
            return Ok(FacebookPostResult::TransientFailure {
                message: format!("Facebook API timeout after {}ms", HTTP_TIMEOUT_MS),
            });
        }
    };

    let status = resp.status_code();
    let raw_body = resp.text().await.unwrap_or_default();
    // L3: never let a token (exact, URL-encoded, truncated, or EAA-shaped)
    // leak into a stored/logged/alerted error message.
    let redacted_body = telegram::redact_token(&raw_body, &fb_secrets.page_access_token);

    // Parse BUC rate-limit usage header on EVERY response (success or failure)
    let buc_usage = resp
        .headers()
        .get("X-Business-Use-Case-Usage")
        .ok()
        .flatten()
        .as_deref()
        .and_then(parse_buc_usage_header)
        .or_else(|| {
            resp.headers()
                .get("X-App-Usage")
                .ok()
                .flatten()
                .as_deref()
                .and_then(parse_buc_usage_header)
        })
        .or_else(|| {
            resp.headers()
                .get("X-Page-Usage")
                .ok()
                .flatten()
                .as_deref()
                .and_then(parse_buc_usage_header)
        });

    if (200..300).contains(&status) {
        let post_id = serde_json::from_str::<GraphPostSuccessResponse>(&raw_body)
            .ok()
            .and_then(|r| r.id)
            .unwrap_or_default();

        Delay::from(StdDuration::from_millis(FACEBOOK_PACE_MS)).await;

        return Ok(FacebookPostResult::Success {
            post_id,
            usage: buc_usage,
        });
    }

    // Parse Graph API error payload
    let error_details = serde_json::from_str::<GraphErrorContainer>(&raw_body)
        .ok()
        .and_then(|c| c.error);

    let error_code = error_details.as_ref().and_then(|e| e.code).unwrap_or(0);
    let error_subcode = error_details.as_ref().and_then(|e| e.error_subcode);
    let error_msg = error_details
        .as_ref()
        .and_then(|e| e.message.as_deref())
        .unwrap_or("Unknown Meta API error");
    let error_msg = telegram::redact_token(error_msg, &fb_secrets.page_access_token);

    Ok(classify_publish_error(
        status,
        error_code,
        error_subcode,
        &error_msg,
        &redacted_body,
        buc_usage.as_ref(),
    ))
}

/// Pure classification of a failed Graph API publish response (H2/L4).
/// Kept separate from `publish_post` (which does network I/O) so the
/// bare-401 / bare-403 rules are unit-testable.
pub(crate) fn classify_publish_error(
    status: u16,
    error_code: i64,
    error_subcode: Option<i64>,
    error_msg: &str,
    redacted_body: &str,
    buc_usage: Option<&BucUsage>,
) -> FacebookPostResult {
    // Server-side transient 5xx without explicit Meta error code
    if status >= 500 && error_code == 0 {
        return FacebookPostResult::TransientFailure {
            message: format!("Meta HTTP {status}: {redacted_body}"),
        };
    }

    // H2: a bare 401 with no parseable Meta error payload is NOT proof of an
    // auth failure — it can be a Meta edge glitch, a proxy/CDN 401, or an
    // HTML error page. Treat it as transient (defer to the next cron) so it
    // can never permanently pause Facebook posting. A genuine auth failure
    // carries a parseable Meta error (code 190/102) and is classified below.
    if status == 401 && error_code == 0 {
        return FacebookPostResult::TransientFailure {
            message: format!("Meta HTTP 401 Unauthorized (unparsable body): {redacted_body}"),
        };
    }

    // L4: a bare 403 with no parseable Meta error payload is, by contrast,
    // a permission denial — treat it as an auth/permission error (with the
    // H2 expiry, so it cannot pause posting forever), matching the handling
    // of a parsed permission error (code 10 / 200..=299) below.
    if status == 403 && error_code == 0 {
        return FacebookPostResult::AuthError {
            message: format!("Meta HTTP 403 Forbidden (unparsable body): {redacted_body}"),
        };
    }

    let action = classify_meta_error(error_code, error_subcode);
    match action {
        MetaErrorAction::RateLimitCooldown { .. } => {
            let cooldown = buc_usage
                .map(|u| u.estimated_time_to_regain_access)
                .filter(|&m| m > 0)
                .unwrap_or(FACEBOOK_RATE_LIMIT_COOLDOWN_FALLBACK_MINS);

            FacebookPostResult::RateLimited {
                cooldown_minutes: cooldown,
            }
        }
        MetaErrorAction::SpamBlock => FacebookPostResult::SpamBlocked,
        MetaErrorAction::DuplicatePost => FacebookPostResult::Duplicate,
        MetaErrorAction::AuthFailure { code, subcode } => FacebookPostResult::AuthError {
            message: format!(
                "Facebook auth failure (code: {code}, subcode: {:?}): {error_msg}",
                subcode
            ),
        },
        MetaErrorAction::PermissionError { code } => FacebookPostResult::AuthError {
            message: format!("Facebook permission error (code: {code}): {error_msg}"),
        },
        MetaErrorAction::TransientError => FacebookPostResult::TransientFailure {
            message: format!("Meta transient error {error_code}: {error_msg}"),
        },
        MetaErrorAction::Unknown { code } => FacebookPostResult::UnknownError {
            message: format!(
                "Meta error {code} (subcode: {:?}): {error_msg}",
                error_subcode
            ),
        },
    }
}

/// Checks whether Facebook publishing is temporarily paused due to rate limiting or auth failure.
///
/// H2: an auth pause expires after `FACEBOOK_AUTH_PAUSE_TTL_HOURS` (24h).
/// An expired pause — or a legacy pause value stored before timestamps
/// were added, which has no reliable start time — is deleted and treated
/// as not paused. A genuine auth failure will re-pause on the next failed
/// post attempt, so expiring a stale pause cannot mask a real problem.
pub async fn is_facebook_paused(
    db: &D1Database,
    session: &D1DatabaseSession,
) -> Result<(bool, Option<String>)> {
    if let Some(reason) = db::get_app_state(session, FACEBOOK_AUTH_PAUSED_KEY).await? {
        match parse_auth_pause_value(&reason) {
            Some((paused_at, message)) => {
                let age = Utc::now().signed_duration_since(paused_at);
                if age.num_hours() >= FACEBOOK_AUTH_PAUSE_TTL_HOURS {
                    console_log!(
                        "Facebook auth pause expired (paused at {}, {}h old); clearing it.",
                        paused_at.to_rfc3339(),
                        age.num_hours()
                    );
                    let _ = db::delete_app_state(db, session, FACEBOOK_AUTH_PAUSED_KEY).await;
                } else {
                    return Ok((true, Some(format!("Authentication paused: {message}"))));
                }
            }
            None => {
                // Legacy value with no timestamp: its age is unknowable, so
                // it must not be allowed to pause posting forever. Clear it;
                // a real auth failure will re-pause on the next attempt.
                console_log!("Facebook auth pause has no timestamp (legacy value); clearing it.");
                let _ = db::delete_app_state(db, session, FACEBOOK_AUTH_PAUSED_KEY).await;
            }
        }
    }

    if let Some(cooldown_until) = db::get_app_state(session, FACEBOOK_COOLDOWN_UNTIL_KEY).await? {
        let now = Utc::now().to_rfc3339();
        if cooldown_until > now {
            return Ok((
                true,
                Some(format!("Rate limit cooldown active until {cooldown_until}")),
            ));
        } else {
            let _ = db::delete_app_state(db, session, FACEBOOK_COOLDOWN_UNTIL_KEY).await;
        }
    }

    Ok((false, None))
}

/// Sends a debounced operational alert to the Telegram admin chat for Facebook issues.
/// Suppresses duplicate alerts within the same category for `FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS`.
pub async fn send_debounced_fb_admin_alert(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    category: &str,
    message: &str,
) -> Result<()> {
    let alert_key = format!("fb_last_alert_at_{category}");
    let now = Utc::now();

    if let Some(last_alert_str) = db::get_app_state(session, &alert_key).await? {
        let debounce_limit =
            (now - Duration::hours(FACEBOOK_ADMIN_ALERT_DEBOUNCE_HOURS)).to_rfc3339();
        if last_alert_str > debounce_limit {
            console_log!(
                "Suppressing duplicate FB admin alert (category: {}, last sent at {})",
                category,
                last_alert_str
            );
            return Ok(());
        }
    }

    let alert_sent = telegram::send_admin_alert(secrets, message)
        .await
        .unwrap_or(false);
    if alert_sent || secrets.telegram_admin_chat_id.is_none() {
        let now_str = now.to_rfc3339();
        let _ = db::set_app_state(db, session, &alert_key, &now_str).await;
        let _ = db::set_app_state(db, session, "fb_last_alert_at", &now_str).await;
    }
    Ok(())
}
