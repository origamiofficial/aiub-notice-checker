//! Telegram / Facebook dispatch phases (Phase C, High #2).
//!
//! `run_inner()` used to inline both dispatch loops (~450 lines) inside
//! the ~1,100-line `run()` god function. The loops now live here as
//! [`dispatch_telegram_phase`] / [`dispatch_facebook_phase`], and
//! [`dispatch_both_phases`] runs them overlapped (O3) and flushes what
//! they collected afterwards (O1 / Medium #7 / High #1).
//!
//! The phases are written so their *decisions* are natively testable:
//!
//! - Every send-result arm is decided by a pure function —
//!   [`telegram_send_step`] / [`facebook_send_step`] — which mutates
//!   the [`DispatchOutcome`] and returns the next step. No I/O, no
//!   logging inside, so the whole decision table runs in native tests.
//! - The network send, the D1 mark building, and the logging are
//!   injected behind small traits ([`TelegramSender`],
//!   [`FacebookSender`], [`MarkSink`], [`DispatchLog`]). The real
//!   sender needs the Worker `Fetch` API, real mark building needs a
//!   `D1Database`, and the `console_*` macros call `web_sys::console`
//!   (which aborts the process on non-wasm targets) — none of those
//!   exist natively, so tests substitute fakes. The production
//!   implementations ([`LiveTelegramSender`], [`LiveFacebookSender`],
//!   [`D1MarkSink`], [`ConsoleDispatchLog`]) are one-liners over the
//!   real calls.
//!
//! Invariants preserved from Phases A/B (do not weaken these):
//!
//! - The Telegram phase does no D1 `.await` while joined.
//! - The Facebook phase collects its D1 writes into the outcome;
//!   they are flushed sequentially after the join.
//! - `tg_auth_paused` / `fb_auth_paused` are checked before the join.

use chrono::{Duration, Utc};
use futures_util::future::join;
use worker::{
    console_error, console_log, console_warn,
    d1::{D1Database, D1DatabaseSession, D1PreparedStatement},
    observability::{SpanStatus, enter_span_async},
    signal::is_near_cpu_limit,
};

use crate::config::{
    FACEBOOK_AUTH_PAUSED_KEY, FACEBOOK_COOLDOWN_UNTIL_KEY, FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS,
    FacebookSecrets, MAX_FACEBOOK_SENDS_PER_RUN, MAX_TELEGRAM_SENDS_PER_RUN, Secrets,
    TELEGRAM_AUTH_PAUSED_KEY,
};
use crate::db;
use crate::errors::AppError;
use crate::facebook::{self, FacebookPostResult};
use crate::models::{D1Metrics, Notice};
use crate::telegram::{self, TelegramSendResult};

/// One queued notification: the notice, its content hash, and the
/// message formatted once at queue time (O5 — both platforms share it).
pub(crate) struct PendingNotification {
    pub notice: Notice,
    pub content_hash: String,
    pub message: String,
}

/// O1/O3: the outcome of one dispatch phase. Sends are complete when
/// this is returned; the D1 marks are only collected statements, to be
/// flushed sequentially by the caller after both phases finish (O3),
/// so no two D1 batches ever run concurrently on the same session.
/// Medium #7 extends the same rule to every other D1 write a phase
/// needs (`app_state` sets, debounced alerts doing get+set): they are
/// collected here and flushed sequentially after the join. The Telegram
/// phase does no D1 `.await` at all while joined.
#[derive(Default)]
pub(crate) struct DispatchOutcome {
    pub sent_count: usize,
    pub deferred_count: usize,
    pub failed_count: usize,
    /// Audit #13: Facebook 506 duplicates previously incremented no
    /// count at all — a run where every post duplicated looked
    /// identical to an empty queue in the Run Summary.
    pub duplicate_count: usize,
    pub attempt_stmts: Vec<D1PreparedStatement>,
    pub notified_stmts: Vec<D1PreparedStatement>,
    pub duplicate_stmts: Vec<D1PreparedStatement>,
    /// D1 `app_state` writes collected during the phase, as
    /// (key, value) pairs — e.g. `fb_cooldown_until`,
    /// `fb_auth_paused`, `tg_auth_paused`.
    pub app_state_sets: Vec<(String, String)>,
    /// Debounced Facebook admin alerts collected during the phase, as
    /// (category, message) pairs.
    pub fb_alerts: Vec<(String, String)>,
    /// Facebook auth-failure message collected during the phase; the
    /// debounced admin alert (category "auth") is sent after the join.
    /// The pause itself is in `app_state_sets`.
    pub fb_auth_alert: Option<String>,
    /// Telegram auth-failure message collected during the Telegram
    /// phase (High #1); the pause is in `app_state_sets` and the
    /// debounced alert is sent after the join.
    pub tg_auth_failure: Option<String>,
}

/// Log sink for the dispatch phases. The production implementation
/// forwards to the `console_*` macros; a test implementation records
/// the lines. The indirection exists because `console_log!` /
/// `console_warn!` / `console_error!` call `web_sys::console`, which
/// aborts the process on non-wasm targets — a phase that logged
/// directly could never execute in a native test on any path that logs.
pub(crate) trait DispatchLog {
    fn warn(&mut self, message: String);
    fn error(&mut self, message: String);
}

/// Production [`DispatchLog`]: the real worker console.
pub(crate) struct ConsoleDispatchLog;

impl DispatchLog for ConsoleDispatchLog {
    fn warn(&mut self, message: String) {
        console_warn!("{message}");
    }

    fn error(&mut self, message: String) {
        console_error!("{message}");
    }
}

/// Sends one Telegram notice. The production implementation calls
/// [`telegram::send_notice_alert`]; tests substitute a scripted fake.
pub(crate) trait TelegramSender {
    async fn send_notice(
        &self,
        secrets: &Secrets,
        message: &str,
    ) -> worker::Result<TelegramSendResult>;
}

/// Production [`TelegramSender`]: the real Bot API call.
pub(crate) struct LiveTelegramSender;

impl TelegramSender for LiveTelegramSender {
    async fn send_notice(
        &self,
        secrets: &Secrets,
        message: &str,
    ) -> worker::Result<TelegramSendResult> {
        telegram::send_notice_alert(secrets, message).await
    }
}

/// Publishes one Facebook post. The production implementation calls
/// [`facebook::publish_post`]; tests substitute a scripted fake.
pub(crate) trait FacebookSender {
    async fn publish(
        &self,
        secrets: &FacebookSecrets,
        message: &str,
    ) -> worker::Result<FacebookPostResult>;
}

/// Production [`FacebookSender`]: the real Graph API call.
pub(crate) struct LiveFacebookSender;

impl FacebookSender for LiveFacebookSender {
    async fn publish(
        &self,
        secrets: &FacebookSecrets,
        message: &str,
    ) -> worker::Result<FacebookPostResult> {
        facebook::publish_post(secrets, message).await
    }
}

/// Builds the D1 mark statements for one dispatch phase and appends
/// them to the outcome. The production implementation binds real
/// statements against the D1 database (which cannot be constructed
/// outside wasm); tests record the calls instead, so mark ordering is
/// assertable natively. A build failure is returned to the phase,
/// which logs it and applies the same log-and-continue / count-as-failed
/// rule the inline code used (M4).
pub(crate) trait MarkSink {
    fn record_tg_attempt(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        now: &str,
    ) -> worker::Result<()>;
    fn record_tg_notified(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        now: &str,
    ) -> worker::Result<()>;
    fn record_fb_attempt(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        now: &str,
    ) -> worker::Result<()>;
    fn record_fb_notified(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        post_id: &str,
        now: &str,
    ) -> worker::Result<()>;
    fn record_fb_duplicate(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        now: &str,
    ) -> worker::Result<()>;
}

/// Production [`MarkSink`]: builds real statements via [`db`].
pub(crate) struct D1MarkSink<'a> {
    db: &'a D1Database,
}

impl<'a> D1MarkSink<'a> {
    pub(crate) fn new(db: &'a D1Database) -> Self {
        Self { db }
    }
}

impl MarkSink for D1MarkSink<'_> {
    fn record_tg_attempt(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        now: &str,
    ) -> worker::Result<()> {
        outcome
            .attempt_stmts
            .push(db::build_tg_attempt_statement(self.db, url, now)?);
        Ok(())
    }

    fn record_tg_notified(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        now: &str,
    ) -> worker::Result<()> {
        outcome.notified_stmts.push(db::build_tg_notified_statement(
            self.db,
            url,
            content_hash,
            now,
        )?);
        Ok(())
    }

    fn record_fb_attempt(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        now: &str,
    ) -> worker::Result<()> {
        outcome
            .attempt_stmts
            .push(db::build_fb_attempt_statement(self.db, url, now)?);
        Ok(())
    }

    fn record_fb_notified(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        post_id: &str,
        now: &str,
    ) -> worker::Result<()> {
        outcome.notified_stmts.push(db::build_fb_notified_statement(
            self.db,
            url,
            content_hash,
            post_id,
            now,
        )?);
        Ok(())
    }

    fn record_fb_duplicate(
        &mut self,
        outcome: &mut DispatchOutcome,
        url: &str,
        content_hash: &str,
        now: &str,
    ) -> worker::Result<()> {
        outcome
            .duplicate_stmts
            .push(db::build_fb_duplicate_handled_statement(
                self.db,
                url,
                content_hash,
                now,
            )?);
        Ok(())
    }
}

/// What the Telegram phase does next after one send attempt, decided
/// purely from the send result: no I/O, no logging, so the whole
/// decision table is natively unit-testable. The shell
/// ([`dispatch_telegram_phase`]) only builds the mark the step asks
/// for, emits the step's log line, and stops or continues. Each
/// variant maps 1:1 to one arm of the old inline `match` in `run()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TelegramSendStep {
    /// Delivered: record the notified mark, then continue.
    RecordNotified,
    /// Rate-limited beyond the inline budget: count deferred, stop the phase.
    DeferredStop { message: String },
    /// Auth failure (401/403): pause + debounced alert collected, count
    /// failed, stop the phase.
    AuthFailureStop { message: String },
    /// Any other send error: count failed, continue with the next notice.
    FailedContinue { message: String },
}

/// Pure Telegram send-result decision (Phase C, Medium #10). Updates
/// the outcome's counts and collected pause/alert state and returns
/// the next step. `message` on the stop/continue variants is the exact
/// log line the shell emits — the step formats it because it owns the
/// decision, the shell only routes it to the log sink.
#[must_use]
pub(crate) fn telegram_send_step(
    outcome: &mut DispatchOutcome,
    url: &str,
    result: &worker::Result<TelegramSendResult>,
) -> TelegramSendStep {
    match result {
        Ok(TelegramSendResult::Sent) => TelegramSendStep::RecordNotified,
        Ok(TelegramSendResult::Deferred) => {
            outcome.deferred_count += 1;
            TelegramSendStep::DeferredStop {
                message: format!(
                    "[Telegram Deferred] Rate limit hit for notice {url}; deferring to next run."
                ),
            }
        }
        Ok(TelegramSendResult::AuthFailure { message }) => {
            // High #1: collect the pause + debounced alert for the
            // post-join flush — no D1 `.await` inside the joined phase
            // (Medium #7), and never retry a dead token within this run.
            let pause_value = facebook::format_auth_pause_value(message);
            outcome
                .app_state_sets
                .push((TELEGRAM_AUTH_PAUSED_KEY.to_string(), pause_value));
            outcome.tg_auth_failure = Some(format!(
                "🚨 AIUB Notice Telegram: Authentication failed: {message}"
            ));
            outcome.failed_count += 1;
            TelegramSendStep::AuthFailureStop {
                message: format!("[Telegram Auth Error] {message}"),
            }
        }
        Err(err) => {
            outcome.failed_count += 1;
            TelegramSendStep::FailedContinue {
                message: format!("[Telegram Send Failed] Telegram send failed for {url}: {err}"),
            }
        }
    }
}

/// What the Facebook phase does next after one publish attempt,
/// decided purely from the publish result — see [`TelegramSendStep`].
/// Each variant maps 1:1 to one arm of the old inline `match` in `run()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FacebookSendStep {
    /// Published: record the notified mark. `cooldown_until` is
    /// `Some((until, max_usage_pct))` when BUC usage is approaching the
    /// limit — the cooldown is already collected in the outcome and the
    /// phase stops after this item.
    RecordNotified {
        post_id: String,
        cooldown_until: Option<(String, u8)>,
    },
    /// Duplicate: record the duplicate-handled mark and continue.
    RecordDuplicate,
    /// Rate-limited: cooldown + debounced alert collected, count
    /// deferred, stop the phase.
    RateLimitedStop { message: String },
    /// Spam/policy block: 12h cooldown + debounced alert collected,
    /// count failed, stop the phase.
    SpamBlockedStop { message: String },
    /// Auth/permission error: pause + direct alert collected, count
    /// failed, stop the phase.
    AuthFailureStop { message: String },
    /// Transient Meta error: count deferred, continue with the next
    /// notice (audit #4) — a flaky or malformed post must not suppress
    /// the rest of the queue.
    TransientContinue { message: String },
    /// Unclassified Meta error: debounced alert collected (M10), count
    /// failed, continue with the next notice (audit #4). No
    /// cooldown/pause is set.
    UnknownContinue { message: String },
    /// Worker-level publish failure: count failed, continue with the
    /// next notice (audit #4).
    WorkerErrorContinue { message: String },
}

/// Pure Facebook publish-result decision (Phase C, Medium #10). See
/// [`telegram_send_step`].
#[must_use]
pub(crate) fn facebook_send_step(
    outcome: &mut DispatchOutcome,
    url: &str,
    result: &worker::Result<FacebookPostResult>,
) -> FacebookSendStep {
    match result {
        Ok(FacebookPostResult::Success { post_id, usage }) => {
            let cooldown_until = match usage {
                Some(u) if u.is_approaching_limit() => {
                    // Audit #14: honour Meta's regain estimate when
                    // it exceeds the 15-minute floor — resuming after
                    // 15 minutes while still throttled just burns
                    // another throttled call before the real cooldown
                    // is set. The estimate is already capped at parse
                    // time (FACEBOOK_BUC_MAX_COOLDOWN_MINS).
                    let cooldown_mins = u.estimated_time_to_regain_access.max(15);
                    let until = (Utc::now() + Duration::minutes(cooldown_mins)).to_rfc3339();
                    // Medium #7: collect the cooldown set for the
                    // post-join flush — no D1 `.await` inside the
                    // joined phase.
                    outcome
                        .app_state_sets
                        .push((FACEBOOK_COOLDOWN_UNTIL_KEY.to_string(), until.clone()));
                    Some((until, u.max_usage()))
                }
                _ => None,
            };
            FacebookSendStep::RecordNotified {
                post_id: post_id.clone(),
                cooldown_until,
            }
        }
        Ok(FacebookPostResult::RateLimited { cooldown_minutes }) => {
            let until = (Utc::now() + Duration::minutes(*cooldown_minutes)).to_rfc3339();
            // Medium #7: collect the cooldown set and the debounced
            // alert for the post-join flush.
            outcome
                .app_state_sets
                .push((FACEBOOK_COOLDOWN_UNTIL_KEY.to_string(), until.clone()));
            outcome.fb_alerts.push((
                "rate_limit".to_string(),
                format!(
                    "⚠️ AIUB Notice Facebook: Rate limit reached. Dispatch paused until {until}."
                ),
            ));
            outcome.deferred_count += 1;
            FacebookSendStep::RateLimitedStop {
                message: format!(
                    "[Facebook Rate Limited] Setting cooldown until {until} ({cooldown_minutes} mins)."
                ),
            }
        }
        Ok(FacebookPostResult::SpamBlocked) => {
            let until =
                (Utc::now() + Duration::hours(FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS)).to_rfc3339();
            // Medium #7: collect the cooldown set and the debounced
            // alert for the post-join flush.
            outcome
                .app_state_sets
                .push((FACEBOOK_COOLDOWN_UNTIL_KEY.to_string(), until.clone()));
            outcome.fb_alerts.push((
                "spam_block".to_string(),
                format!(
                    "🚨 AIUB Notice Facebook: Meta blocked post for policy/spam (error 368). Dispatch paused until {until}. Please inspect Facebook Page!"
                ),
            ));
            outcome.failed_count += 1;
            FacebookSendStep::SpamBlockedStop {
                message: format!(
                    "[Facebook Spam Block] Meta error 368: policy/spam block. Pausing until {until}."
                ),
            }
        }
        Ok(FacebookPostResult::Duplicate) => {
            // Medium #7: collect the debounced alert for the post-join
            // flush.
            outcome.fb_alerts.push((
                "duplicate_post".to_string(),
                format!(
                    "⚠️ AIUB Notice Facebook: Consecutive duplicate rejected by Meta (error 506): {url}"
                ),
            ));
            FacebookSendStep::RecordDuplicate
        }
        Ok(FacebookPostResult::AuthError { message }) => {
            // H2: store the pause with a timestamp so
            // is_facebook_paused() can expire it after
            // FACEBOOK_AUTH_PAUSE_TTL_HOURS. Medium #7: collect the
            // pause set and the debounced admin alert for the
            // post-join flush.
            let pause_value = facebook::format_auth_pause_value(message);
            outcome
                .app_state_sets
                .push((FACEBOOK_AUTH_PAUSED_KEY.to_string(), pause_value));
            outcome.fb_auth_alert = Some(format!(
                "🚨 AIUB Notice Facebook: Authentication paused: {message}"
            ));
            outcome.failed_count += 1;
            FacebookSendStep::AuthFailureStop {
                message: format!("[Facebook Auth Error] {message}"),
            }
        }
        Ok(FacebookPostResult::TransientFailure { message }) => {
            outcome.deferred_count += 1;
            FacebookSendStep::TransientContinue {
                message: format!("[Facebook Transient] Error: {message}; will retry at next cron."),
            }
        }
        Ok(FacebookPostResult::UnknownError { message }) => {
            // M10: an unclassified Meta error previously got no admin
            // alert, no pause and no cooldown — it failed silently
            // every run until someone noticed the logs. Route it
            // through the existing debounced FB admin alert (6h,
            // category "unknown_error") so it is seen once without
            // spamming. No cooldown/pause is set: the error is
            // unclassified, so pausing could suppress good posts for a
            // transient Meta glitch.
            // Medium #7: collect the alert for the post-join flush.
            outcome.fb_alerts.push((
                "unknown_error".to_string(),
                format!(
                    "⚠️ AIUB Notice Facebook: Unclassified Meta error, dispatch paused for this run (no cooldown set): {message}"
                ),
            ));
            outcome.failed_count += 1;
            FacebookSendStep::UnknownContinue {
                message: format!("[Facebook Unknown Error] {message}"),
            }
        }
        Err(err) => {
            outcome.failed_count += 1;
            FacebookSendStep::WorkerErrorContinue {
                message: format!("[Facebook Worker Error] Failed to publish notice {url}: {err}"),
            }
        }
    }
}

/// Telegram dispatch phase, extracted from `run_inner()` (Phase C,
/// High #2). Sends at most `MAX_TELEGRAM_SENDS_PER_RUN` queued
/// notices, oldest first (the caller reverses the queue). The only
/// D1 interaction is mark building through `marks` — no D1 `.await`
/// happens here — and the only logging is through `log`. Every send
/// result is decided by [`telegram_send_step`]; this shell only builds
/// the mark the step asks for, emits its log line, and stops or
/// continues. Returns a default outcome when the queue is empty or
/// Telegram is paused.
pub(crate) async fn dispatch_telegram_phase<S, M, L>(
    secrets: &Secrets,
    notifications: Vec<PendingNotification>,
    paused: bool,
    sender: &S,
    marks: &mut M,
    log: &mut L,
) -> DispatchOutcome
where
    S: TelegramSender,
    M: MarkSink,
    L: DispatchLog,
{
    let mut outcome = DispatchOutcome::default();
    if notifications.is_empty() || paused {
        return outcome;
    }

    for item in notifications.into_iter().take(MAX_TELEGRAM_SENDS_PER_RUN) {
        if is_near_cpu_limit() {
            log.warn(
                "[Telegram Deferred] Near CPU limit during Telegram dispatch; deferring remaining."
                    .to_string(),
            );
            outcome.deferred_count += 1;
            break;
        }

        // O1: accumulate the attempt mark; flushed after the phase
        // instead of one round-trip per send.
        let attempt_now = Utc::now().to_rfc3339();
        if let Err(err) = marks.record_tg_attempt(&mut outcome, &item.notice.url, &attempt_now) {
            log.warn(format!(
                "[Telegram Attempt Mark Failed] Could not build attempt mark for {}: {}; send continues.",
                item.notice.url, err
            ));
        }

        let send_result = sender.send_notice(secrets, &item.message).await;
        match telegram_send_step(&mut outcome, &item.notice.url, &send_result) {
            TelegramSendStep::RecordNotified => {
                // O1: accumulate the notified mark. M4: a failed mark
                // must not abort the rest of dispatch — the send
                // already happened, so log it and continue; the notice
                // may re-send on the next run, which is safer than
                // dropping the remaining queue.
                let now = Utc::now().to_rfc3339();
                match marks.record_tg_notified(
                    &mut outcome,
                    &item.notice.url,
                    &item.content_hash,
                    &now,
                ) {
                    Ok(()) => outcome.sent_count += 1,
                    Err(err) => {
                        log.error(format!(
                            "[Telegram Mark Failed] Send succeeded for {} but building its notified mark failed: {}; it may re-send on the next run.",
                            item.notice.url, err
                        ));
                        outcome.failed_count += 1;
                    }
                }
            }
            TelegramSendStep::DeferredStop { message } => {
                log.warn(message);
                break;
            }
            TelegramSendStep::AuthFailureStop { message } => {
                log.error(message);
                break;
            }
            TelegramSendStep::FailedContinue { message } => {
                log.error(message);
            }
        }
    }

    outcome
}

/// Facebook dispatch phase, extracted from `run_inner()` (Phase C,
/// High #2). Same shape as [`dispatch_telegram_phase`]: at most
/// `MAX_FACEBOOK_SENDS_PER_RUN` queued posts, D1 writes collected
/// into the outcome for the post-join flush, every publish result
/// decided by [`facebook_send_step`].
pub(crate) async fn dispatch_facebook_phase<S, M, L>(
    fb_secrets: &FacebookSecrets,
    notifications: Vec<PendingNotification>,
    paused: bool,
    sender: &S,
    marks: &mut M,
    log: &mut L,
) -> DispatchOutcome
where
    S: FacebookSender,
    M: MarkSink,
    L: DispatchLog,
{
    let mut outcome = DispatchOutcome::default();
    if notifications.is_empty() || paused {
        return outcome;
    }

    for item in notifications.into_iter().take(MAX_FACEBOOK_SENDS_PER_RUN) {
        if is_near_cpu_limit() {
            log.warn(
                "[Facebook Deferred] Near CPU limit during Facebook dispatch; deferring remaining."
                    .to_string(),
            );
            outcome.deferred_count += 1;
            break;
        }

        // O1: accumulate the attempt mark; flushed after the phase
        // instead of one round-trip per send.
        let attempt_now = Utc::now().to_rfc3339();
        if let Err(err) = marks.record_fb_attempt(&mut outcome, &item.notice.url, &attempt_now) {
            log.warn(format!(
                "[Facebook Attempt Mark Failed] Could not build attempt mark for {}: {}; send continues.",
                item.notice.url, err
            ));
        }

        // O5: the message was formatted once at queue time; the
        // Facebook phase no longer re-formats it.
        let post_outcome = sender.publish(fb_secrets, &item.message).await;
        match facebook_send_step(&mut outcome, &item.notice.url, &post_outcome) {
            FacebookSendStep::RecordNotified {
                post_id,
                cooldown_until,
            } => {
                // O1: accumulate the notified mark. M4: same
                // log-and-continue rule as the Telegram mark — a
                // failed mark must not abort the remaining dispatch.
                let now = Utc::now().to_rfc3339();
                match marks.record_fb_notified(
                    &mut outcome,
                    &item.notice.url,
                    &item.content_hash,
                    &post_id,
                    &now,
                ) {
                    Ok(()) => outcome.sent_count += 1,
                    Err(err) => {
                        log.error(format!(
                            "[Facebook Mark Failed] Post {post_id} published for {} but building its notified mark failed: {}; it may re-send (and hit a 506 duplicate) on the next run.",
                            item.notice.url, err
                        ));
                        outcome.failed_count += 1;
                    }
                }

                if let Some((until, max_usage)) = cooldown_until {
                    log.warn(format!(
                        "[Facebook BUC High] Usage reached {max_usage}%; cooling down until {until}."
                    ));
                    break;
                }
            }
            FacebookSendStep::RecordDuplicate => {
                log.warn(format!(
                    "[Facebook Duplicate] Error 506 duplicate post for {}; marking handled.",
                    item.notice.url
                ));
                // O1: accumulate the duplicate-handled mark.
                // M4: log-and-continue — a failed mark must not abort
                // the remaining dispatch.
                let now = Utc::now().to_rfc3339();
                match marks.record_fb_duplicate(
                    &mut outcome,
                    &item.notice.url,
                    &item.content_hash,
                    &now,
                ) {
                    Ok(()) => outcome.duplicate_count += 1,
                    Err(err) => {
                        log.error(format!(
                            "[Facebook Duplicate Mark Failed] Could not build duplicate-handled mark for {}: {}; it will be retried on the next run.",
                            item.notice.url, err
                        ));
                        outcome.failed_count += 1;
                    }
                }
            }
            FacebookSendStep::RateLimitedStop { message } => {
                log.warn(message);
                break;
            }
            FacebookSendStep::SpamBlockedStop { message } => {
                log.error(message);
                break;
            }
            FacebookSendStep::AuthFailureStop { message } => {
                log.error(message);
                break;
            }
            FacebookSendStep::TransientContinue { message } => {
                log.warn(message);
            }
            FacebookSendStep::UnknownContinue { message } => {
                log.error(message);
            }
            FacebookSendStep::WorkerErrorContinue { message } => {
                log.error(message);
            }
        }
    }

    outcome
}

/// Executes one batch of prepared statements. Production calls
/// [`db::execute_batch`]; tests substitute a scripted fake. The seam
/// exists so the flush ordering and failure counting in
/// [`flush_dispatch_marks`] are testable without a live D1 (audit
/// #15).
pub(crate) trait BatchExecutor {
    async fn execute_batch(&self, stmts: Vec<D1PreparedStatement>) -> worker::Result<D1Metrics>;
}

/// Production [`BatchExecutor`]: the real D1 batch call.
pub(crate) struct LiveBatchExecutor<'a> {
    session: &'a D1DatabaseSession,
}

impl<'a> LiveBatchExecutor<'a> {
    pub(crate) fn new(session: &'a D1DatabaseSession) -> Self {
        Self { session }
    }
}

impl BatchExecutor for LiveBatchExecutor<'_> {
    async fn execute_batch(&self, stmts: Vec<D1PreparedStatement>) -> worker::Result<D1Metrics> {
        db::execute_batch(self.session, stmts).await
    }
}

/// Performs one dispatch side effect: an `app_state` write or a
/// debounced admin alert. Production calls the real D1/alert
/// functions; tests substitute a recording fake. The seam exists so
/// the flush ordering and non-fatal failure handling in
/// [`flush_dispatch_side_effects`] are natively testable (audit #15)
/// — the side-effect inputs are plain strings, unlike the mark
/// flush, whose `D1PreparedStatement` values cannot be constructed
/// natively.
pub(crate) trait SideEffectSink {
    async fn set_app_state(&self, key: &str, value: &str) -> worker::Result<D1Metrics>;
    async fn send_fb_alert(&self, category: &str, message: &str) -> worker::Result<()>;
    async fn send_fb_auth_alert(&self, message: &str) -> worker::Result<()>;
    async fn send_tg_auth_alert(&self, message: &str) -> worker::Result<()>;
}

/// Production [`SideEffectSink`]: the real D1 writes and admin alerts.
pub(crate) struct LiveSideEffectSink<'a> {
    db: &'a D1Database,
    session: &'a D1DatabaseSession,
    secrets: &'a Secrets,
}

impl<'a> LiveSideEffectSink<'a> {
    pub(crate) fn new(
        db: &'a D1Database,
        session: &'a D1DatabaseSession,
        secrets: &'a Secrets,
    ) -> Self {
        Self {
            db,
            session,
            secrets,
        }
    }
}

impl SideEffectSink for LiveSideEffectSink<'_> {
    async fn set_app_state(&self, key: &str, value: &str) -> worker::Result<D1Metrics> {
        db::set_app_state(self.db, self.session, key, value).await
    }

    async fn send_fb_alert(&self, category: &str, message: &str) -> worker::Result<()> {
        facebook::send_debounced_fb_admin_alert(
            self.db,
            self.session,
            self.secrets,
            category,
            message,
        )
        .await
    }

    async fn send_fb_auth_alert(&self, message: &str) -> worker::Result<()> {
        facebook::send_debounced_fb_admin_alert(
            self.db,
            self.session,
            self.secrets,
            "auth",
            message,
        )
        .await
    }

    async fn send_tg_auth_alert(&self, message: &str) -> worker::Result<()> {
        crate::send_debounced_tg_auth_alert(self.db, self.session, self.secrets, message).await
    }
}

/// Folds one mark-batch result into the flush metrics (audit #15).
/// A failed batch contributes its whole statement count — those marks
/// did not apply, so the affected notices may re-send. Pure, so the
/// failure-counting rule is natively testable even though the flush
/// itself needs real `D1PreparedStatement` values.
pub(crate) fn fold_mark_batch_result(
    metrics: &mut D1Metrics,
    result: &worker::Result<D1Metrics>,
    count: usize,
) {
    match result {
        Ok(m) => {
            metrics.rows_read += m.rows_read;
            metrics.rows_written += m.rows_written;
            metrics.failed_statements += m.failed_statements;
        }
        Err(_) => metrics.failed_statements += count,
    }
}

/// O1: flushes accumulated dispatch marks in one batch per mark kind
/// (attempt, notified, duplicate), instead of one round-trip per send.
/// A batch that fails entirely is logged and its statement count added
/// to `failed_statements` — those marks did not apply, so the affected
/// notices may re-send on the next run (safe direction, same as M4).
/// Takes the mark vectors out of `outcome` (`mem::take`) so the caller
/// can pass the rest of the outcome to `flush_dispatch_side_effects`.
/// The batch execution and the logging go through the injected
/// [`BatchExecutor`] / [`DispatchLog`] so the flush is testable
/// without a live D1 (audit #15).
async fn flush_dispatch_marks(
    executor: &impl BatchExecutor,
    log: &mut impl DispatchLog,
    label: &str,
    outcome: &mut DispatchOutcome,
) -> D1Metrics {
    let mut metrics = D1Metrics::default();
    for (kind, stmts) in [
        ("attempt", std::mem::take(&mut outcome.attempt_stmts)),
        ("notified", std::mem::take(&mut outcome.notified_stmts)),
        ("duplicate", std::mem::take(&mut outcome.duplicate_stmts)),
    ] {
        if stmts.is_empty() {
            continue;
        }
        let count = stmts.len();
        let result = executor.execute_batch(stmts).await;
        if let Err(err) = &result {
            log.error(format!(
                "[Dispatch Mark Flush Failed] {label} {kind} batch ({count} marks) failed entirely: {err}; affected notices may re-send on the next run."
            ));
        }
        fold_mark_batch_result(&mut metrics, &result, count);
    }
    metrics
}

/// Medium #7 / High #1: flushes the D1 writes and admin alerts a
/// dispatch phase collected while joined. Runs sequentially after the
/// join, so no D1 `.await` ever happens inside the overlapped phases.
/// Every failure is logged and non-fatal — the next run re-evaluates
/// (a missed cooldown set re-sets on the next rate limit; a missed
/// alert timestamp re-alerts; a missed pause set re-pauses on the next
/// failed send). The writes/alerts and the logging go through the
/// injected [`SideEffectSink`] / [`DispatchLog`] so the flush order
/// and non-fatal handling are natively testable (audit #15).
pub(crate) async fn flush_dispatch_side_effects(
    sink: &impl SideEffectSink,
    log: &mut impl DispatchLog,
    label: &str,
    outcome: DispatchOutcome,
) {
    for (key, value) in outcome.app_state_sets {
        if let Err(err) = sink.set_app_state(&key, &value).await {
            // Medium #3: classify the D1 failure instead of logging a
            // bare worker error.
            log.warn(format!(
                "[Dispatch State Flush Failed] {label}: {}",
                AppError::D1(format!("failed to set app_state {key}: {err}"))
            ));
        }
    }
    for (category, message) in outcome.fb_alerts {
        if let Err(err) = sink.send_fb_alert(&category, &message).await {
            log.warn(format!(
                "[Dispatch Alert Flush Failed] {label}: failed to send debounced FB admin alert (category: {category}): {err}"
            ));
        }
    }
    if let Some(message) = outcome.fb_auth_alert
        && let Err(err) = sink.send_fb_auth_alert(&message).await
    {
        // Audit #11: the auth alert used to be sent directly here and
        // only `Err` was handled — `Ok(false)` (deferred on a Telegram
        // 429, or no admin chat) was treated as delivered and the alert
        // was lost for the 24h pause. Routed through the debounced
        // alert like every other Facebook alert, so a deferral does
        // not record its timestamp and retries on the next run.
        log.warn(format!(
            "[Dispatch Alert Flush Failed] {label}: failed to send debounced Facebook auth admin alert (category: auth): {err}"
        ));
    }
    if let Some(message) = outcome.tg_auth_failure
        && let Err(err) = sink.send_tg_auth_alert(&message).await
    {
        log.warn(format!(
            "[Dispatch Alert Flush Failed] {label}: failed to send debounced Telegram auth alert: {err}"
        ));
    }
}

/// Per-platform dispatch counts returned by [`dispatch_both_phases`]
/// for the Run Summary (audit #13). The combined dispatch used to
/// return only D1 metrics, so a run where every Facebook post hit a
/// 506 duplicate looked identical to an empty queue.
#[derive(Default)]
pub(crate) struct DispatchCounts {
    pub tg_sent: usize,
    pub tg_deferred: usize,
    pub tg_failed: usize,
    pub fb_sent: usize,
    pub fb_deferred: usize,
    pub fb_failed: usize,
    pub fb_duplicates: usize,
}

/// Runs both dispatch phases overlapped (O3) and flushes everything
/// they collected, sequentially, after the join (O1 / Medium #7 /
/// High #1). The pause pre-checks are sequential D1 reads before the
/// overlap: neither phase may do D1 reads/writes while the other is
/// sending (Medium #7). Returns the D1 metrics from the mark flushes
/// and the per-platform send counts.
pub(crate) async fn dispatch_both_phases(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    mut tg_notifications: Vec<PendingNotification>,
    mut fb_notifications: Vec<PendingNotification>,
) -> worker::Result<(D1Metrics, DispatchCounts)> {
    // Audit #12: a pause-check D1 error must not abort dispatch for
    // the other platform — each check is independent, so a failed read
    // logs and treats that platform as not paused instead of
    // propagating with `?` and losing the whole run (and its Run
    // Summary). A genuinely paused platform re-pauses on the next
    // failed send, so failing open here is the safe direction.
    let fb_paused = if !fb_notifications.is_empty() && secrets.facebook.is_some() {
        match facebook::is_facebook_paused(db, session).await {
            Ok(paused) => paused,
            Err(err) => {
                console_warn!(
                    "[Facebook Pause Check Failed] Could not read fb_auth_paused: {err}; treating Facebook as not paused."
                );
                (false, None)
            }
        }
    } else {
        (false, None)
    };
    if fb_paused.0 {
        console_log!(
            "[Facebook Dispatch Skipped] {}",
            fb_paused.1.as_deref().unwrap_or("paused")
        );
    }

    // High #1: same pre-check for Telegram — a 401/403 pause from
    // a previous run skips Telegram dispatch before any send.
    let tg_paused = if !tg_notifications.is_empty() {
        match telegram::is_telegram_paused(db, session).await {
            Ok(paused) => paused,
            Err(err) => {
                console_warn!(
                    "[Telegram Pause Check Failed] Could not read tg_auth_paused: {err}; treating Telegram as not paused."
                );
                (false, None)
            }
        }
    } else {
        (false, None)
    };
    if tg_paused.0 {
        console_log!(
            "[Telegram Dispatch Skipped] {}",
            tg_paused.1.as_deref().unwrap_or("paused")
        );
    }

    tg_notifications.reverse();
    fb_notifications.reverse();
    let db_ref = db;
    let secrets_ref = secrets;
    let fb_secrets_ref = secrets.facebook.as_ref();
    let fb_paused_flag = fb_paused.0;
    let tg_paused_flag = tg_paused.0;

    // O3: both phases run concurrently; each only collects D1 marks.
    let tg_future = async move {
        if tg_notifications.is_empty() || tg_paused_flag {
            return DispatchOutcome::default();
        }
        enter_span_async("dispatch_telegram_notifications", |span| async move {
            span.set_attribute("queued", tg_notifications.len() as i64);
            let mut marks = D1MarkSink::new(db_ref);
            let mut log = ConsoleDispatchLog;
            let outcome = dispatch_telegram_phase(
                secrets_ref,
                tg_notifications,
                tg_paused_flag,
                &LiveTelegramSender,
                &mut marks,
                &mut log,
            )
            .await;
            span.set_attribute("sent", outcome.sent_count as i64);
            span.set_attribute("deferred", outcome.deferred_count as i64);
            span.set_attribute("failed", outcome.failed_count as i64);
            if outcome.failed_count > 0 {
                span.set_status(SpanStatus::Error(Some(format!(
                    "{} sends failed",
                    outcome.failed_count
                ))));
            } else {
                span.set_status(SpanStatus::Ok);
            }
            outcome
        })
        .await
    };

    let fb_future = async move {
        if fb_notifications.is_empty() || fb_secrets_ref.is_none() || fb_paused_flag {
            return DispatchOutcome::default();
        }
        let fb_secrets = fb_secrets_ref.expect("Facebook secrets present (checked above)");
        enter_span_async("dispatch_facebook_notifications", |span| async move {
            span.set_attribute("queued", fb_notifications.len() as i64);
            let mut marks = D1MarkSink::new(db_ref);
            let mut log = ConsoleDispatchLog;
            let outcome = dispatch_facebook_phase(
                fb_secrets,
                fb_notifications,
                fb_paused_flag,
                &LiveFacebookSender,
                &mut marks,
                &mut log,
            )
            .await;
            span.set_attribute("sent", outcome.sent_count as i64);
            span.set_attribute("deferred", outcome.deferred_count as i64);
            span.set_attribute("failed", outcome.failed_count as i64);
            span.set_attribute("duplicates", outcome.duplicate_count as i64);
            if outcome.failed_count > 0 {
                span.set_status(SpanStatus::Error(Some(format!(
                    "{} sends failed",
                    outcome.failed_count
                ))));
            } else {
                span.set_status(SpanStatus::Ok);
            }
            outcome
        })
        .await
    };

    // O3: run both phases concurrently, then flush the collected
    // marks sequentially (O1) — Telegram first, then Facebook.
    // Audit #17: the phases return their outcome directly — they
    // contain no fallible call — so there is no error arm to hit here.
    let (mut tg_outcome, mut fb_outcome) = join(tg_future, fb_future).await;
    // Audit #13: capture the counts before the flushes consume the
    // outcomes — the flush takes them by value.
    let counts = DispatchCounts {
        tg_sent: tg_outcome.sent_count,
        tg_deferred: tg_outcome.deferred_count,
        tg_failed: tg_outcome.failed_count,
        fb_sent: fb_outcome.sent_count,
        fb_deferred: fb_outcome.deferred_count,
        fb_failed: fb_outcome.failed_count,
        fb_duplicates: fb_outcome.duplicate_count,
    };
    // Audit #15: the flushes run through the injected executor/sink/
    // log seams so they are testable without a live D1.
    let executor = LiveBatchExecutor::new(session);
    let sink = LiveSideEffectSink::new(db, session, secrets);
    let mut log = ConsoleDispatchLog;
    let tg_flush = flush_dispatch_marks(&executor, &mut log, "Telegram", &mut tg_outcome).await;
    let fb_flush = flush_dispatch_marks(&executor, &mut log, "Facebook", &mut fb_outcome).await;
    // Medium #7 / High #1: flush the D1 writes and admin alerts
    // the phases collected while joined, sequentially — Telegram
    // first, then Facebook.
    flush_dispatch_side_effects(&sink, &mut log, "Telegram", tg_outcome).await;
    flush_dispatch_side_effects(&sink, &mut log, "Facebook", fb_outcome).await;
    // Audit #10: while Telegram is paused, no send ever fails, so the
    // debounced auth alert in the flush above is never attempted — yet
    // its log line promises a retry "on the next run". Attempt it on
    // every run while paused: with a dead token it fails (and retries
    // next run); once the token is fixed the alert delivers promptly
    // and the debounce timestamp is recorded.
    if tg_paused_flag
        && let Some(reason) = &tg_paused.1
        && let Err(err) = crate::send_debounced_tg_auth_alert(
            db,
            session,
            secrets,
            &format!("🚨 AIUB Notice Telegram: {reason}"),
        )
        .await
    {
        console_warn!(
            "[Dispatch Alert Flush Failed] Telegram: failed to send debounced Telegram auth alert while paused: {err}"
        );
    }
    let mut metrics = D1Metrics::default();
    metrics.rows_read += tg_flush.rows_read + fb_flush.rows_read;
    metrics.rows_written += tg_flush.rows_written + fb_flush.rows_written;
    metrics.failed_statements += tg_flush.failed_statements + fb_flush.failed_statements;
    Ok((metrics, counts))
}
