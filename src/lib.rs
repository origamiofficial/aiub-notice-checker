use chrono::{DateTime, Duration, Utc};
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
use worker::{
    console_error, console_log, console_warn,
    d1::{D1Database, D1DatabaseSession, D1PreparedStatement},
    event,
    observability::{enter_span, enter_span_async, SpanStatus},
    signal::is_near_cpu_limit,
    Env, ScheduleContext, ScheduledEvent,
};

pub mod config;
mod db;
pub mod facebook;
pub mod models;
pub mod scraper;
mod telegram;
#[cfg(test)]
mod tests;
pub mod version;

use config::{
    Secrets, AIUB_OUTAGE_ALERTED_KEY, AIUB_OUTAGE_ALERT_THRESHOLD_MINS, AIUB_OUTAGE_DOWN_SINCE_KEY,
    DETAIL_FAILURE_ALERT_DEBOUNCE_HOURS, DETAIL_FAILURE_LAST_ALERT_KEY, DETAIL_FAILURE_MIN_SAMPLE,
    DETAIL_FAILURE_RATIO, DETAIL_RETRY_COOLDOWN_MINUTES, EMPTY_LISTING_ALERT_DEBOUNCE_HOURS,
    EMPTY_LISTING_ALERT_KEY, FACEBOOK_AUTH_PAUSED_KEY, FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS,
    MAX_DETAIL_FETCHES_PER_RUN, MAX_FACEBOOK_SENDS_PER_RUN, MAX_TELEGRAM_SENDS_PER_RUN,
    MIN_LISTING_DATE_COVERAGE, MIN_LISTING_DESC_COVERAGE,
};
use models::{format_notice_message, titles_equivalent, D1Metrics, Notice, NoticeRow};
use scraper::{DetailFetchOutcome, ParsedDetail};

const DETAIL_CONCURRENCY: usize = 3;

/// Formats an actionable admin alert distinguishing DOM parser failures from upstream AIUB/network errors.
pub fn format_detail_failure_alert(
    attempted_count: usize,
    http_error_count: usize,
    parse_error_count: usize,
    succeeded_count: usize,
    cpu_skipped_count: usize,
    failure_diagnostics: &[String],
) -> String {
    if parse_error_count >= 1 && parse_error_count >= http_error_count {
        let mut msg = format!(
            "AIUB Notice\n\n⚠️ Most sampled detail pages failed DOM validation. The site parser may need an update.\n\n\
             Diagnostics:\n\
             • Sampled pages: {}\n\
             • DOM parse failed: {}\n\
             • HTTP/Network errors: {}\n\
             • Succeeded: {}\n\
             • CPU skipped: {}",
            attempted_count,
            parse_error_count,
            http_error_count,
            succeeded_count,
            cpu_skipped_count
        );
        if !failure_diagnostics.is_empty() {
            msg.push_str("\n\nFailed URLs:\n");
            for diag in failure_diagnostics.iter().take(3) {
                msg.push_str(&format!("• {diag}\n"));
            }
        }
        msg
    } else if http_error_count > 0 {
        let mut msg = format!(
            "AIUB Notice\n\n⚠️ Detail page fetch failed due to upstream AIUB server or network errors (AIUB may be down or timing out).\n\n\
             Diagnostics:\n\
             • Sampled pages: {}\n\
             • HTTP/Network errors: {}\n\
             • DOM parse failed: {}\n\
             • Succeeded: {}\n\
             • CPU skipped: {}",
            attempted_count,
            http_error_count,
            parse_error_count,
            succeeded_count,
            cpu_skipped_count
        );
        if !failure_diagnostics.is_empty() {
            msg.push_str("\n\nFailed URLs:\n");
            for diag in failure_diagnostics.iter().take(3) {
                msg.push_str(&format!("• {diag}\n"));
            }
        }
        msg
    } else {
        format!(
            "AIUB Notice\n\n⚠️ Detail page fetches deferred: Cloudflare Worker approached CPU limits ({} skipped).",
            cpu_skipped_count
        )
    }
}

pub fn format_listing_skip_message(err: &str) -> String {
    format!("[Listing Fetch Skipped] AIUB listing page unreachable: {err}. Skipping this run.")
}

/// Formats a human-readable duration (e.g., "1 hour 15 minutes", "2 days 3 hours", "< 1 minute").
pub fn format_duration_human(duration: Duration) -> String {
    let total_secs = duration.num_seconds().max(0);
    if total_secs < 60 {
        return "< 1 minute".to_string();
    }

    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{} day{}", days, if days == 1 { "" } else { "s" }));
    }
    if hours > 0 {
        parts.push(format!(
            "{} hour{}",
            hours,
            if hours == 1 { "" } else { "s" }
        ));
    }
    if minutes > 0 {
        parts.push(format!(
            "{} minute{}",
            minutes,
            if minutes == 1 { "" } else { "s" }
        ));
    }

    if parts.is_empty() {
        "< 1 minute".to_string()
    } else {
        parts.join(" ")
    }
}

pub fn format_outage_alert_message(duration_str: &str) -> String {
    format!(
        "⚠️ AIUB Notice Checker: AIUB website has been unreachable for {duration_str}. Notice checks are paused until the site recovers."
    )
}

pub fn format_recovery_alert_message(duration_str: &str) -> String {
    format!(
        "✅ AIUB Notice Checker: AIUB website is back online after a total outage of {duration_str}. Notice checking has resumed."
    )
}

async fn handle_upstream_outage(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    now: DateTime<Utc>,
) -> worker::Result<()> {
    // O7: both outage keys in one round-trip instead of two sequential
    // get_app_state calls.
    let states = db::get_app_states(
        session,
        &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
    )
    .await?;
    let down_since_str = states.get(AIUB_OUTAGE_DOWN_SINCE_KEY).cloned();
    let down_since = match down_since_str {
        Some(ref s) if !s.is_empty() => match DateTime::parse_from_rfc3339(s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                // L5: do NOT overwrite a corrupt stored value with `now` —
                // that would restart the 60-minute alert clock. Preserve
                // the stored value and treat the outage as already started
                // (alert on schedule instead of delaying it by up to an
                // hour). The corrupt value is logged for diagnosis.
                console_warn!(
                    "[Outage State Warning] Stored {} value is not valid RFC3339 (value: {:?}); treating outage as already started.",
                    AIUB_OUTAGE_DOWN_SINCE_KEY,
                    s
                );
                now - Duration::minutes(AIUB_OUTAGE_ALERT_THRESHOLD_MINS)
            }
        },
        _ => {
            // First failed run of this outage: record the start time.
            // L5: a failed write is propagated (not silently ignored with
            // `let _ =`), so the caller logs it and the next run retries
            // instead of the alert silently never firing.
            let now_str = now.to_rfc3339();
            db::set_app_state(db, session, AIUB_OUTAGE_DOWN_SINCE_KEY, &now_str).await?;
            now
        }
    };

    let elapsed = now.signed_duration_since(down_since);
    if elapsed.num_minutes() >= AIUB_OUTAGE_ALERT_THRESHOLD_MINS {
        let already_alerted =
            states.get(AIUB_OUTAGE_ALERTED_KEY).map(String::as_str) == Some("true");

        if !already_alerted {
            let duration_str = format_duration_human(elapsed);
            let alert_msg = format_outage_alert_message(&duration_str);
            let alert_sent = telegram::send_admin_alert(secrets, &alert_msg)
                .await
                .unwrap_or(false);
            if alert_sent || secrets.telegram_admin_chat_id.is_none() {
                db::set_app_state(db, session, AIUB_OUTAGE_ALERTED_KEY, "true").await?;
            }
        }
    }

    Ok(())
}

/// M1: a recovery is only a recovery when the listing fetch actually
/// parsed notices. A 0-notice fetch (DOM change) is not the site coming
/// back, so it must not send a recovery alert or clear the outage state.
async fn handle_upstream_recovery(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    now: DateTime<Utc>,
    listing_count: usize,
) -> worker::Result<()> {
    // O7: both outage keys in one round-trip instead of two sequential
    // get_app_state calls.
    let states = db::get_app_states(
        session,
        &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
    )
    .await?;
    let down_since_str = states.get(AIUB_OUTAGE_DOWN_SINCE_KEY).cloned();
    let Some(down_since_str) = down_since_str else {
        return Ok(());
    };
    if down_since_str.is_empty() {
        return Ok(());
    }

    let already_alerted = states.get(AIUB_OUTAGE_ALERTED_KEY).map(String::as_str) == Some("true");

    if !already_alerted {
        // The outage never reached the alert threshold, so no recovery
        // message is owed — just clear the (short) outage state.
        db::delete_app_state_keys(
            db,
            session,
            &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
        )
        .await?;
        return Ok(());
    }

    if listing_count == 0 {
        // Not a recovery: the fetch succeeded but parsed nothing. Keep the
        // outage state so the next genuinely successful run can recover.
        console_warn!(
            "[Recovery Skipped] Listing fetch parsed 0 notices; not treating this as an AIUB recovery."
        );
        return Ok(());
    }

    let Ok(down_since) = DateTime::parse_from_rfc3339(&down_since_str) else {
        console_warn!(
            "[Recovery State Warning] Stored {} value is not valid RFC3339 (value: {:?}); clearing outage state without a recovery alert.",
            AIUB_OUTAGE_DOWN_SINCE_KEY,
            down_since_str
        );
        db::delete_app_state_keys(
            db,
            session,
            &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
        )
        .await?;
        return Ok(());
    };

    let total_duration = now.signed_duration_since(down_since.with_timezone(&Utc));
    let duration_str = format_duration_human(total_duration);
    let recovery_msg = format_recovery_alert_message(&duration_str);

    // M1: do NOT delete the outage keys until the recovery alert actually
    // lands. If Telegram fails (or is rate-limited), the keys stay and the
    // next run retries the recovery alert instead of losing it silently.
    let alert_sent = match telegram::send_admin_alert(secrets, &recovery_msg).await {
        Ok(true) => true,
        Ok(false) => secrets.telegram_admin_chat_id.is_none(),
        Err(err) => {
            console_warn!(
                "[Recovery Alert Failed] Failed to send recovery alert: {err}; will retry on the next run."
            );
            false
        }
    };

    if alert_sent {
        db::delete_app_state_keys(
            db,
            session,
            &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
        )
        .await?;
    } else {
        console_warn!(
            "[Recovery Alert Deferred] Recovery alert was not delivered; outage state kept for retry."
        );
    }

    Ok(())
}

/// O2: elapsed milliseconds between two Utc timestamps, for per-phase
/// timing in the Run Summary.
fn elapsed_ms_since(start: DateTime<Utc>) -> i64 {
    Utc::now().signed_duration_since(start).num_milliseconds()
}

/// Pure debounce check (M2): returns true when an alert with the given
/// last-sent RFC3339 timestamp may be sent again — i.e. no timestamp is
/// stored, the stored value is corrupt (treat as expired), or it is older
/// than `debounce_hours`.
fn is_debounce_expired(
    last_alert_str: Option<&str>,
    now: DateTime<Utc>,
    debounce_hours: i64,
) -> bool {
    let Some(last_alert_str) = last_alert_str else {
        return true;
    };
    let Ok(last_alert) = DateTime::parse_from_rfc3339(last_alert_str) else {
        return true;
    };
    last_alert.with_timezone(&Utc) <= now - Duration::hours(debounce_hours)
}

/// M2: sends the "0 notices parsed" admin alert at most once per
/// `EMPTY_LISTING_ALERT_DEBOUNCE_HOURS`, instead of on every 5-minute run
/// during a DOM break. The timestamp is only recorded when the alert was
/// actually delivered (or no admin chat is configured).
async fn send_debounced_empty_listing_alert(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
) -> worker::Result<()> {
    let now = Utc::now();
    if let Some(last_alert_str) = db::get_app_state(session, EMPTY_LISTING_ALERT_KEY).await? {
        if !is_debounce_expired(
            Some(&last_alert_str),
            now,
            EMPTY_LISTING_ALERT_DEBOUNCE_HOURS,
        ) {
            console_log!(
                "Suppressing duplicate empty-listing alert (last sent at {})",
                last_alert_str
            );
            return Ok(());
        }
    }

    let alert_sent = match telegram::send_admin_alert(
        secrets,
        "⚠️ AIUB Notice Checker: 0 notices found on listing page 1. DOM layout may have changed.",
    )
    .await
    {
        Ok(true) => true,
        Ok(false) => secrets.telegram_admin_chat_id.is_none(),
        Err(err) => {
            console_warn!(
                "[Empty Listing Alert Failed] Failed to send empty-listing alert: {err}; will retry on the next run."
            );
            false
        }
    };

    if alert_sent {
        let now_str = now.to_rfc3339();
        db::set_app_state(db, session, EMPTY_LISTING_ALERT_KEY, &now_str).await?;
    }
    Ok(())
}

#[event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    enter_span_async("scheduled_notice_check", |span| async move {
        if let Err(err) = run(&env).await {
            console_error!("Scheduled run failed: {}", err);
            span.set_status(SpanStatus::Error(Some(err.to_string())));
        } else {
            span.set_status(SpanStatus::Ok);
        }
    })
    .await;
}

/// O1/O3: the outcome of one dispatch phase. Sends are complete when
/// this is returned; the D1 marks are only collected statements, to be
/// flushed sequentially by the caller after both phases finish (O3),
/// so no two D1 batches ever run concurrently on the same session.
#[derive(Default)]
struct DispatchOutcome {
    sent_count: usize,
    deferred_count: usize,
    failed_count: usize,
    attempt_stmts: Vec<D1PreparedStatement>,
    notified_stmts: Vec<D1PreparedStatement>,
    duplicate_stmts: Vec<D1PreparedStatement>,
}

/// O1: flushes accumulated dispatch marks in one batch per mark kind
/// (attempt, notified, duplicate), instead of one round-trip per send.
/// A batch that fails entirely is logged and its statement count added
/// to `failed_statements` — those marks did not apply, so the affected
/// notices may re-send on the next run (safe direction, same as M4).
async fn flush_dispatch_marks(
    session: &D1DatabaseSession,
    label: &str,
    attempt_stmts: Vec<D1PreparedStatement>,
    notified_stmts: Vec<D1PreparedStatement>,
    duplicate_stmts: Vec<D1PreparedStatement>,
) -> D1Metrics {
    let mut metrics = D1Metrics::default();
    for (kind, stmts) in [
        ("attempt", attempt_stmts),
        ("notified", notified_stmts),
        ("duplicate", duplicate_stmts),
    ] {
        if stmts.is_empty() {
            continue;
        }
        let count = stmts.len();
        match db::execute_batch(session, stmts).await {
            Ok(m) => {
                metrics.rows_read += m.rows_read;
                metrics.rows_written += m.rows_written;
                metrics.failed_statements += m.failed_statements;
            }
            Err(err) => {
                console_error!(
                    "[Dispatch Mark Flush Failed] {label} {kind} batch ({count} marks) failed entirely: {err}; affected notices may re-send on the next run."
                );
                metrics.failed_statements += count;
            }
        }
    }
    metrics
}

async fn run(env: &Env) -> worker::Result<()> {
    let secrets = Secrets::from_env(env)?;
    let database = env.d1("DB")?;

    // Start a sequentially consistent D1 session anchored to the primary instance.
    let session = database.with_session(Some("first-primary"))?;
    let mut total_d1 = D1Metrics::default();

    // O2: per-phase wall-time accounting for the Run Summary. Chrono
    // timestamps are used instead of std::time::Instant: this is
    // logging-grade timing, and it works unchanged on wasm.
    let run_started = Utc::now();
    let mut phase_started = run_started;

    // 1. Fetch the latest notices on listing page 1.
    let listing_notices: Option<Vec<Notice>> =
        enter_span_async("fetch_listing", |span| async move {
            let listing_url = scraper::listing_page_url(1);
            span.set_attribute("url", listing_url.as_str());
            let html = match scraper::fetch_html(&listing_url).await {
                Ok(h) => h,
                Err(err) => {
                    let err_msg = err.to_string();
                    console_warn!("{}", format_listing_skip_message(&err_msg));
                    span.set_attribute("upstream_available", "false");
                    span.set_attribute("skip_reason", err_msg.as_str());
                    span.set_status(SpanStatus::Ok);
                    return Ok::<Option<Vec<Notice>>, worker::Error>(None);
                }
            };
            span.set_attribute("upstream_available", "true");
            let parsed = scraper::parse_listing_html(&html);
            span.set_attribute("notices_found", parsed.len() as i64);
            span.set_attribute("html_bytes", html.len() as i64);
            span.set_status(SpanStatus::Ok);
            Ok::<Option<Vec<Notice>>, worker::Error>(Some(parsed))
        })
        .await?;

    let Some(listing_notices) = listing_notices else {
        if let Err(err) = handle_upstream_outage(&database, &session, &secrets, Utc::now()).await {
            console_warn!("[Outage State Warning] Failed to update outage state: {err}");
        }
        return Ok(());
    };

    let fetch_listing_ms = elapsed_ms_since(phase_started);
    phase_started = Utc::now();

    // M1: recovery is only evaluated against a listing that actually
    // parsed notices; a 0-notice fetch keeps the outage state.
    if let Err(err) = handle_upstream_recovery(
        &database,
        &session,
        &secrets,
        Utc::now(),
        listing_notices.len(),
    )
    .await
    {
        console_warn!("[Recovery State Warning] Failed to handle outage recovery: {err}");
    }

    if listing_notices.is_empty() {
        console_warn!(
            "[Listing Parse Failed] 0 notices parsed from listing page 1. DOM layout may have changed."
        );
        // M2: debounced — at most one alert per EMPTY_LISTING_ALERT_DEBOUNCE_HOURS.
        if let Err(err) = send_debounced_empty_listing_alert(&database, &session, &secrets).await {
            console_warn!(
                "[Empty Listing Alert Warning] Failed to handle empty-listing alert: {err}"
            );
        }
        return Ok(());
    }

    // Deduplicate listing links while preserving top-to-bottom order.
    let mut seen_links = HashSet::with_capacity(listing_notices.len());
    let mut unique_listing: Vec<Notice> = Vec::with_capacity(listing_notices.len());
    for item in listing_notices {
        if seen_links.insert(item.url.clone()) {
            unique_listing.push(item);
        }
    }

    // Listing coverage sanity check.
    let total_listed = unique_listing.len() as f64;
    let date_coverage = unique_listing
        .iter()
        .filter(|n| n.published_date.is_some())
        .count() as f64
        / total_listed;
    let desc_coverage = unique_listing
        .iter()
        .filter(|n| !n.description.is_empty())
        .count() as f64
        / total_listed;

    if date_coverage < MIN_LISTING_DATE_COVERAGE || desc_coverage < MIN_LISTING_DESC_COVERAGE {
        console_warn!(
            "Listing selector coverage degraded: date_coverage={:.2}, desc_coverage={:.2}",
            date_coverage,
            desc_coverage
        );
    }

    let links: Vec<String> = unique_listing.iter().map(|n| n.url.clone()).collect();

    // 2. Load existing D1 rows strictly for the current page 1 notices.
    let session_ref = &session;
    let links_ref = &links;
    let (existing_map, read_metrics) =
        enter_span_async("load_existing_from_d1", |span| async move {
            span.set_attribute("queried_links", links_ref.len() as i64);
            match db::get_notices_by_links(session_ref, links_ref).await {
                Ok((map, m)) => {
                    span.set_attribute("matched_rows", map.len() as i64);
                    span.set_attribute("rows_read", m.rows_read as i64);
                    span.set_status(SpanStatus::Ok);
                    Ok::<(HashMap<String, NoticeRow>, D1Metrics), worker::Error>((map, m))
                }
                Err(err) => {
                    console_error!(
                        "[D1 Query Failed] Failed to query existing notices: {}",
                        err
                    );
                    span.set_status(SpanStatus::Error(Some(err.to_string())));
                    Err(err)
                }
            }
        })
        .await?;

    total_d1.rows_read += read_metrics.rows_read;
    total_d1.rows_written += read_metrics.rows_written;

    let load_existing_ms = elapsed_ms_since(phase_started);
    phase_started = Utc::now();

    // 3. Select up to MAX_DETAIL_FETCHES_PER_RUN (5) detail pages strictly from the visible notices.
    //    See select_detail_targets() (M15) for the Priority A/B/C rules.
    let retry_cutoff_iso =
        (Utc::now() - Duration::minutes(DETAIL_RETRY_COOLDOWN_MINUTES)).to_rfc3339();

    let detail_targets = enter_span("select_detail_targets", |span| {
        let selection = select_detail_targets(&unique_listing, &existing_map, &retry_cutoff_iso);
        span.set_attribute("new_targets", selection.new_count as i64);
        span.set_attribute("backfill_targets", selection.backfill_count as i64);
        span.set_attribute("rotation_targets", selection.rotation_count as i64);
        span.set_attribute("total_targets", selection.targets.len() as i64);
        span.set_status(SpanStatus::Ok);
        selection.targets
    });

    // 4. Fetch the selected detail pages with bounded concurrency and CPU budget awareness.
    let targets_ref = &detail_targets;
    let detail_results = enter_span_async("fetch_detail_pages", |span| async move {
        span.set_attribute("requested", targets_ref.len() as i64);

        let fetched: Vec<(String, DetailFetchOutcome)> =
            stream::iter(targets_ref.iter().cloned())
                .map(|link| async move {
                    if is_near_cpu_limit() {
                        console_warn!(
                            "[Detail Fetch Skipped] Near CPU limit before fetching detail page {}; deferring.",
                            link
                        );
                        return (link, DetailFetchOutcome::CpuSkipped);
                    }
                    match scraper::fetch_html(&link).await {
                        Ok(html) => {
                            let html_len = html.len();
                            match scraper::parse_detail_with_reason(&html, &link) {
                                Ok(parsed) => (link, DetailFetchOutcome::Success(parsed)),
                                Err(reason) => {
                                    console_warn!(
                                        "[Detail Parse Failed] link={}, reason={}, html_len={}",
                                        link,
                                        reason,
                                        html_len
                                    );
                                    (link, DetailFetchOutcome::ParseError { reason, html_len })
                                }
                            }
                        }
                        Err(err) => {
                            console_warn!(
                                "[Detail HTTP Failed] link={}, error={}",
                                link,
                                err
                            );
                            (link, DetailFetchOutcome::HttpError(err.to_string()))
                        }
                    }
                })
                .buffer_unordered(DETAIL_CONCURRENCY)
                .collect()
                .await;

        let mut succeeded = 0usize;
        let mut http_errs = 0usize;
        let mut parse_errs = 0usize;
        let mut cpu_skips = 0usize;

        for (_, outcome) in &fetched {
            match outcome {
                DetailFetchOutcome::Success(_) => succeeded += 1,
                DetailFetchOutcome::HttpError(_) => http_errs += 1,
                DetailFetchOutcome::ParseError { .. } => parse_errs += 1,
                DetailFetchOutcome::CpuSkipped => cpu_skips += 1,
            }
        }

        span.set_attribute("succeeded", succeeded as i64);
        span.set_attribute("http_errors", http_errs as i64);
        span.set_attribute("parse_errors", parse_errs as i64);
        span.set_attribute("cpu_skipped", cpu_skips as i64);

        if parse_errs > 0 || http_errs > 0 {
            span.set_status(SpanStatus::Error(Some(format!(
                "{}/{} succeeded (http_errors={}, parse_errors={}, cpu_skipped={})",
                succeeded,
                targets_ref.len(),
                http_errs,
                parse_errs,
                cpu_skips
            ))));
        } else {
            span.set_status(SpanStatus::Ok);
        }

        console_log!(
            "[Detail Fetch Summary] requested={}, succeeded={}, http_errors={}, parse_errors={}, cpu_skipped={}",
            targets_ref.len(),
            succeeded,
            http_errs,
            parse_errs,
            cpu_skips
        );

        fetched
    })
    .await;

    let attempted_count = detail_results.len();
    let detail_fetch_ms = elapsed_ms_since(phase_started);
    phase_started = Utc::now();
    let mut detail_succeeded_count = 0usize;
    let mut detail_http_error_count = 0usize;
    let mut detail_parse_error_count = 0usize;
    let mut detail_cpu_skipped_count = 0usize;
    let mut failure_diagnostics: Vec<String> = Vec::new();

    for (link, outcome) in &detail_results {
        match outcome {
            DetailFetchOutcome::Success(_) => {
                detail_succeeded_count += 1;
            }
            DetailFetchOutcome::HttpError(err) => {
                detail_http_error_count += 1;
                failure_diagnostics.push(format!("{link} (HTTP: {err})"));
            }
            DetailFetchOutcome::ParseError { reason, html_len } => {
                detail_parse_error_count += 1;
                failure_diagnostics.push(format!("{link} (DOM: {reason}, len={html_len})"));
            }
            DetailFetchOutcome::CpuSkipped => {
                detail_cpu_skipped_count += 1;
                failure_diagnostics.push(format!("{link} (CPU: limit approached, deferred)"));
            }
        }
    }

    let total_failed =
        detail_http_error_count + detail_parse_error_count + detail_cpu_skipped_count;
    if attempted_count >= DETAIL_FAILURE_MIN_SAMPLE
        && (total_failed as f64 / attempted_count as f64) >= DETAIL_FAILURE_RATIO
    {
        console_warn!(
            "High detail page failure rate: {}/{} failed (http_errors={}, parse_errors={}, cpu_skipped={})",
            total_failed,
            attempted_count,
            detail_http_error_count,
            detail_parse_error_count,
            detail_cpu_skipped_count
        );

        let alert_message = format_detail_failure_alert(
            attempted_count,
            detail_http_error_count,
            detail_parse_error_count,
            detail_succeeded_count,
            detail_cpu_skipped_count,
            &failure_diagnostics,
        );

        // M5: debounced — a sustained AIUB outage previously alerted on
        // every 5-minute run.
        if let Err(err) =
            send_debounced_detail_failure_alert(&database, &session, &secrets, &alert_message).await
        {
            console_warn!(
                "[Detail Failure Alert Warning] Failed to handle detail-failure alert: {err}"
            );
        }
    }

    let mut detail_map: HashMap<String, Option<ParsedDetail>> =
        HashMap::with_capacity(detail_results.len());
    for (link, outcome) in detail_results {
        let opt = match outcome {
            DetailFetchOutcome::Success(detail) => Some(detail),
            _ => None,
        };
        detail_map.insert(link, opt);
    }

    // 5. Reconcile state, compute hashes, build D1 batch upserts, and queue notifications.
    struct PendingNotification {
        notice: Notice,
        content_hash: String,
        // O5: the formatted message, computed once at queue time. The
        // Telegram and Facebook formats are identical, so formatting
        // twice per notice (once per phase) was pure waste. The
        // is_edited flag used to live here too, but dispatch no longer
        // reads it — it only exists while formatting the message.
        message: String,
    }

    let mut statements = Vec::new();
    let mut tg_notifications: Vec<PendingNotification> = Vec::new();
    let mut fb_notifications: Vec<PendingNotification> = Vec::new();
    // (url, new_hash, tg_needs_sync, fb_needs_sync) — see H1.
    let mut silent_sync_links: Vec<(String, String, bool, bool)> = Vec::new();

    // Cold-start guard: if D1 is completely empty and we scraped a full page of notices,
    // this is a fresh database seed — write all rows silently and populate tracking columns
    // so neither Telegram nor Facebook are flooded.
    // M3: emptiness is measured with COUNT(*) over the whole table, not
    // inferred from page-1 matches — a DB holding only older rows is not
    // empty. On a COUNT failure, fall back to the page-1 heuristic
    // rather than failing the run.
    let db_is_empty = match db::count_notices(&session).await {
        Ok(n) => n == 0,
        Err(err) => {
            console_warn!(
                "[Cold-Start Check Failed] COUNT(*) failed ({}); falling back to page-1 emptiness heuristic.",
                err
            );
            existing_map.is_empty()
        }
    };
    let is_empty_db_seed = db_is_empty && unique_listing.len() >= 10;
    if is_empty_db_seed {
        console_log!(
            "Empty-database cold-start detected ({} notices). Seeding silently without Telegram or Facebook notifications.",
            unique_listing.len()
        );
    }

    for mut item in unique_listing {
        let existing = existing_map.get(&item.url);
        let listing_title = item.title.clone();

        if let Some(row) = existing {
            if item.title.contains("...")
                && !has_utf8_mojibake(&row.title)
                && titles_equivalent(&item.title, &row.title)
            {
                item.title = row.title.clone();
            }
            if item.published_date.is_none() && row.published_date.is_some() {
                item.published_date = row.published_date.clone();
            }
        }

        let detail_outcome = detail_map.get(&item.url);
        let detail_attempted = detail_outcome.is_some();
        let mut matched_legacy_sticky = false;

        match (detail_outcome, existing) {
            (Some(Some(detail)), _) => {
                if let Some(full_title) = &detail.title {
                    item.title = full_title.clone();
                }
                if item.published_date.is_none() && detail.published_date.is_some() {
                    item.published_date = detail.published_date.clone();
                }
                if let Some(row) = existing {
                    if !detail.legacy_sticky_body.is_empty()
                        && clean_mojibake_eq(row.body_text.trim(), detail.legacy_sticky_body.trim())
                    {
                        matched_legacy_sticky = true;
                    }
                }
                item.body_text = detail.body_text.clone();
                item.attachments = detail.attachments.clone();
                item.detail_fetched = true;
            }
            (Some(None), Some(row)) => {
                item.body_text = row.body_text.clone();
                item.attachments = row.parsed_attachments();
                item.detail_fetched = false;
            }
            (Some(None), None) => {
                item.detail_fetched = false;
            }
            (None, Some(row)) => {
                item.body_text = row.body_text.clone();
                item.attachments = row.parsed_attachments();
                item.detail_fetched = row.detail_fetched_at.is_some();
            }
            (None, None) => {
                item.detail_fetched = false;
            }
        }

        let new_hash = item.content_hash();
        // O5: serialize the attachments once; the upsert and the
        // silent-sync check previously each serialized them again.
        let attachments_json = item.attachments_json();

        // Determine whether this row needs a full upsert or just a lightweight timestamp update.
        let (needs_db_write, use_lightweight_update) = match existing {
            None => (true, false),
            Some(row) => {
                let content_changed = row.content_hash != new_hash
                    || row.title != item.title
                    || row.description != item.description
                    || row.published_date != item.published_date;

                if detail_attempted && !content_changed {
                    (true, true)
                } else if detail_attempted || content_changed {
                    (true, false)
                } else {
                    (false, false)
                }
            }
        };

        // Silent hash sync for non-edit backfills — see silent_sync_flags()
        // (M15) for the eligibility rules.
        let (silent_sync_tg, silent_sync_fb) = match existing {
            Some(row) => silent_sync_flags(
                row,
                &listing_title,
                &item,
                &new_hash,
                &attachments_json,
                matched_legacy_sticky,
            ),
            None => (false, false),
        };
        let silent_hash_sync = silent_sync_tg || silent_sync_fb;

        if needs_db_write {
            if use_lightweight_update {
                let stmt =
                    db::build_lightweight_detail_update(&database, &item.url, item.detail_fetched)?;
                statements.push(stmt);
            } else {
                let stmt = db::build_upsert_statement(
                    &database,
                    &item,
                    &new_hash,
                    &attachments_json,
                    detail_attempted,
                )?;
                statements.push(stmt);
            }
        }

        if silent_hash_sync {
            silent_sync_links.push((item.url.clone(), new_hash, silent_sync_tg, silent_sync_fb));
            continue;
        }

        // During a cold-start seed, all brand-new rows are written to D1
        // but no notifications are queued for either platform.
        if is_empty_db_seed {
            continue;
        }

        match existing {
            None => {
                if item.detail_fetched || detail_attempted {
                    // O5: format once — both platforms get the identical
                    // message for a new notice (is_edited is false for
                    // both), so the second push clones the string
                    // instead of re-running the formatter.
                    let message = format_notice_message(&item, false);
                    tg_notifications.push(PendingNotification {
                        notice: item.clone(),
                        content_hash: new_hash.clone(),
                        message: message.clone(),
                    });
                    fb_notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        message,
                    });
                }
            }
            Some(row) => {
                // Notification eligibility (M15): the Telegram and
                // Facebook rules are identical given each platform's own
                // sent_at / last-notified-hash, so one pure function
                // serves both phases.
                let mut tg_message: Option<(bool, String)> = None;
                if let Some(is_edited) = platform_notification(
                    row.tg_sent_at.as_deref(),
                    row.tg_last_notified_hash.as_deref(),
                    item.detail_fetched,
                    detail_attempted,
                    row.detail_checked_at.is_some(),
                    &new_hash,
                ) {
                    // O5: format here, once; the Facebook push below
                    // reuses this message when its is_edited matches.
                    let message = format_notice_message(&item, is_edited);
                    tg_message = Some((is_edited, message.clone()));
                    tg_notifications.push(PendingNotification {
                        notice: item.clone(),
                        content_hash: new_hash.clone(),
                        message,
                    });
                }

                if let Some(is_edited) = platform_notification(
                    row.fb_sent_at.as_deref(),
                    row.fb_last_notified_hash.as_deref(),
                    item.detail_fetched,
                    detail_attempted,
                    row.detail_checked_at.is_some(),
                    &new_hash,
                ) {
                    let message = match &tg_message {
                        Some((tg_edited, m)) if *tg_edited == is_edited => m.clone(),
                        _ => format_notice_message(&item, is_edited),
                    };
                    fb_notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        message,
                    });
                }
            }
        }
    }

    let silent_backfills = silent_sync_links.len();

    // 6. Execute D1 batch upserts on the session.
    // M8: if any upsert failed, D1 state is uncertain — dispatch is
    // deferred to the next run instead of notifying for rows that may
    // not have been written.
    let mut upsert_failed = false;
    if !statements.is_empty() {
        let stmt_count = statements.len();
        let batch_metrics = enter_span_async("d1_batch_upsert", |span| async move {
            span.set_attribute("statements", stmt_count as i64);
            match db::execute_batch(session_ref, statements).await {
                Ok(m) => {
                    span.set_attribute("rows_read", m.rows_read as i64);
                    span.set_attribute("rows_written", m.rows_written as i64);
                    span.set_attribute("failed_statements", m.failed_statements as i64);
                    span.set_status(SpanStatus::Ok);
                    Ok::<D1Metrics, worker::Error>(m)
                }
                Err(err) => {
                    console_error!(
                        "[D1 Batch Upsert Failed] Batch upsert of {} statements failed: {}",
                        stmt_count,
                        err
                    );
                    span.set_status(SpanStatus::Error(Some(err.to_string())));
                    Err(err)
                }
            }
        })
        .await?;

        if batch_metrics.failed_statements > 0 {
            upsert_failed = true;
            console_error!(
                "[D1 Batch Partial Failure] {}/{} upsert statements failed; deferring Telegram/Facebook dispatch to the next run.",
                batch_metrics.failed_statements,
                stmt_count
            );
        }

        total_d1.rows_read += batch_metrics.rows_read;
        total_d1.rows_written += batch_metrics.rows_written;
        total_d1.failed_statements += batch_metrics.failed_statements;
    }

    // Cold-start seed: populate tracking fields for both platforms so subsequent runs
    // never flood historical notices onto Telegram or Facebook.
    if is_empty_db_seed {
        let now = Utc::now().to_rfc3339();
        let mut seed_stmts = Vec::with_capacity(links.len());
        for link in &links {
            let stmt = worker::query!(
                &database,
                "UPDATE notices SET tg_sent_at = ?1, tg_last_notified_hash = content_hash, \
                 fb_sent_at = ?1, fb_last_notified_hash = content_hash WHERE link = ?2",
                &now,
                link,
            )?;
            seed_stmts.push(stmt);
        }
        let seed_metrics = db::execute_batch(&session, seed_stmts).await?;
        total_d1.rows_read += seed_metrics.rows_read;
        total_d1.rows_written += seed_metrics.rows_written;
    }

    // Apply silent hash updates after the batch upsert completes in a single batch.
    // H1: push each platform's sync statement ONLY when that platform
    // needs sync. build_tg_hash_sync_statement() (unlike the old
    // build_tg_notified_statement() used here) never sets tg_sent_at, so a
    // never-sent Telegram notice can no longer be silently marked sent.
    if !silent_sync_links.is_empty() {
        let mut sync_stmts = Vec::with_capacity(silent_sync_links.len() * 2);
        for (url, hash, tg_needs_sync, fb_needs_sync) in &silent_sync_links {
            if *tg_needs_sync {
                sync_stmts.push(db::build_tg_hash_sync_statement(&database, url, hash)?);
            }
            if *fb_needs_sync {
                sync_stmts.push(db::build_fb_hash_sync_statement(&database, url, hash)?);
            }
        }
        let sync_stmt_count = sync_stmts.len();
        let sync_metrics = db::execute_batch(&session, sync_stmts).await?;
        if sync_metrics.failed_statements > 0 {
            // M8: a failed silent sync is not fatal — the next run
            // re-evaluates — but it must be visible, not swallowed.
            console_warn!(
                "[Silent Sync Partial Failure] {}/{} hash-sync statements failed; affected notices will be re-evaluated on the next run.",
                sync_metrics.failed_statements,
                sync_stmt_count
            );
        }
        total_d1.rows_read += sync_metrics.rows_read;
        total_d1.rows_written += sync_metrics.rows_written;
        total_d1.failed_statements += sync_metrics.failed_statements;
    }

    let upsert_ms = elapsed_ms_since(phase_started);
    phase_started = Utc::now();

    // 7+8. Dispatch queued notifications.
    // O3: the Telegram and Facebook phases overlap. Their wall time was
    // 10x1.0s + 5x1.5s = 17.5s of sequential pacing; overlapped it is the
    // longer of the two, with per-platform pacing unchanged.
    // O1: each phase only *collects* D1 mark statements while sending;
    // the marks are flushed sequentially after both phases finish, so no
    // two D1 batches ever run concurrently on the same session.
    // M8: both phases are skipped entirely when the upsert batch
    // partially failed.
    if !upsert_failed {
        // Sequential D1 read before the overlap: the Facebook phase must
        // not do D1 reads while Telegram is sending.
        let fb_paused = if !fb_notifications.is_empty() && secrets.facebook.is_some() {
            facebook::is_facebook_paused(&database, &session).await?
        } else {
            (false, None)
        };
        if fb_paused.0 {
            console_log!(
                "[Facebook Dispatch Skipped] {}",
                fb_paused.1.as_deref().unwrap_or("paused")
            );
        }

        tg_notifications.reverse();
        fb_notifications.reverse();
        let db_ref = &database;
        let secrets_ref = &secrets;
        let session_ref = &session;
        let fb_secrets_ref = secrets.facebook.as_ref();
        let fb_paused_flag = fb_paused.0;

        // O3: both phases run concurrently; each only collects D1 marks.
        let tg_future = async move {
            if tg_notifications.is_empty() {
                return Ok::<DispatchOutcome, worker::Error>(DispatchOutcome::default());
            }
            enter_span_async("dispatch_telegram_notifications", |span| async move {
                span.set_attribute("queued", tg_notifications.len() as i64);
                let mut outcome = DispatchOutcome::default();

                for item in tg_notifications.into_iter().take(MAX_TELEGRAM_SENDS_PER_RUN) {
                    if is_near_cpu_limit() {
                        console_warn!(
                            "[Telegram Deferred] Near CPU limit during Telegram dispatch; deferring remaining."
                        );
                        outcome.deferred_count += 1;
                        break;
                    }

                    // O1: accumulate the attempt mark; flushed after the
                    // phase instead of one round-trip per send.
                    let attempt_now = Utc::now().to_rfc3339();
                    match db::build_tg_attempt_statement(db_ref, &item.notice.url, &attempt_now)
                    {
                        Ok(stmt) => outcome.attempt_stmts.push(stmt),
                        Err(err) => {
                            console_warn!(
                                "[Telegram Attempt Mark Failed] Could not build attempt mark for {}: {}; send continues.",
                                item.notice.url, err
                            );
                        }
                    }

                    match telegram::send_notice_alert(secrets_ref, &item.message).await {
                        Ok(true) => {
                            // O1: accumulate the notified mark. M4: a
                            // failed mark must not abort the rest of
                            // dispatch — the send already happened, so
                            // log it and continue; the notice may
                            // re-send on the next run, which is safer
                            // than dropping the remaining queue.
                            let now = Utc::now().to_rfc3339();
                            match db::build_tg_notified_statement(
                                db_ref,
                                &item.notice.url,
                                &item.content_hash,
                                &now,
                            ) {
                                Ok(stmt) => {
                                    outcome.notified_stmts.push(stmt);
                                    outcome.sent_count += 1;
                                }
                                Err(err) => {
                                    console_error!(
                                        "[Telegram Mark Failed] Send succeeded for {} but building its notified mark failed: {}; it may re-send on the next run.",
                                        item.notice.url,
                                        err
                                    );
                                    outcome.failed_count += 1;
                                }
                            }
                        }
                        Ok(false) => {
                            console_warn!(
                                "[Telegram Deferred] Rate limit hit for notice {}; deferring to next run.",
                                item.notice.url
                            );
                            outcome.deferred_count += 1;
                            break;
                        }
                        Err(err) => {
                            console_error!(
                                "[Telegram Send Failed] Telegram send failed for {}: {}",
                                item.notice.url,
                                err
                            );
                            outcome.failed_count += 1;
                        }
                    }
                }

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
                Ok::<DispatchOutcome, worker::Error>(outcome)
            })
            .await
        };

        let fb_future = async move {
            if fb_notifications.is_empty() || fb_secrets_ref.is_none() || fb_paused_flag {
                return Ok::<DispatchOutcome, worker::Error>(DispatchOutcome::default());
            }
            let fb_secrets = fb_secrets_ref.expect("Facebook secrets present (checked above)");
            enter_span_async("dispatch_facebook_notifications", |span| async move {
                span.set_attribute("queued", fb_notifications.len() as i64);
                let mut outcome = DispatchOutcome::default();

                for item in fb_notifications.into_iter().take(MAX_FACEBOOK_SENDS_PER_RUN) {
                    if is_near_cpu_limit() {
                        console_warn!(
                            "[Facebook Deferred] Near CPU limit during Facebook dispatch; deferring remaining."
                        );
                        outcome.deferred_count += 1;
                        break;
                    }

                    // O1: accumulate the attempt mark; flushed after the
                    // phase instead of one round-trip per send.
                    let attempt_now = Utc::now().to_rfc3339();
                    match db::build_fb_attempt_statement(db_ref, &item.notice.url, &attempt_now)
                    {
                        Ok(stmt) => outcome.attempt_stmts.push(stmt),
                        Err(err) => {
                            console_warn!(
                                "[Facebook Attempt Mark Failed] Could not build attempt mark for {}: {}; send continues.",
                                item.notice.url, err
                            );
                        }
                    }

                    // O5: the message was formatted once at queue time;
                    // the Facebook phase no longer re-formats it.
                    let message = &item.message;
                    let post_outcome = facebook::publish_post(fb_secrets, message).await;

                    match post_outcome {
                        Ok(facebook::FacebookPostResult::Success { post_id, usage }) => {
                            // O1: accumulate the notified mark. M4: same
                            // log-and-continue rule as the Telegram
                            // mark — a failed mark must not abort the
                            // remaining dispatch.
                            let now = Utc::now().to_rfc3339();
                            match db::build_fb_notified_statement(
                                db_ref,
                                &item.notice.url,
                                &item.content_hash,
                                &post_id,
                                &now,
                            ) {
                                Ok(stmt) => {
                                    outcome.notified_stmts.push(stmt);
                                    outcome.sent_count += 1;
                                }
                                Err(err) => {
                                    console_error!(
                                        "[Facebook Mark Failed] Post {} published for {} but building its notified mark failed: {}; it may re-send (and hit a 506 duplicate) on the next run.",
                                        post_id,
                                        item.notice.url,
                                        err
                                    );
                                    outcome.failed_count += 1;
                                }
                            }

                            if let Some(u) = usage {
                                if u.is_approaching_limit() {
                                    let until =
                                        (Utc::now() + Duration::minutes(15)).to_rfc3339();
                                    let _ = db::set_app_state(
                                        db_ref,
                                        session_ref,
                                        "fb_cooldown_until",
                                        &until,
                                    )
                                    .await;
                                    console_warn!(
                                        "[Facebook BUC High] Usage reached {}%; cooling down until {}.",
                                        u.max_usage(),
                                        until
                                    );
                                    break;
                                }
                            }
                        }
                        Ok(facebook::FacebookPostResult::RateLimited { cooldown_minutes }) => {
                            let until =
                                (Utc::now() + Duration::minutes(cooldown_minutes)).to_rfc3339();
                            let _ = db::set_app_state(
                                db_ref,
                                session_ref,
                                "fb_cooldown_until",
                                &until,
                            )
                            .await;
                            console_warn!(
                                "[Facebook Rate Limited] Setting cooldown until {} ({} mins).",
                                until,
                                cooldown_minutes
                            );
                            let _ = facebook::send_debounced_fb_admin_alert(
                                db_ref,
                                session_ref,
                                secrets_ref,
                                "rate_limit",
                                &format!(
                                    "⚠️ AIUB Notice Facebook: Rate limit reached. Dispatch paused until {until}."
                                ),
                            )
                            .await;
                            outcome.deferred_count += 1;
                            break;
                        }
                        Ok(facebook::FacebookPostResult::SpamBlocked) => {
                            let until = (Utc::now()
                                + Duration::hours(FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS))
                            .to_rfc3339();
                            let _ = db::set_app_state(
                                db_ref,
                                session_ref,
                                "fb_cooldown_until",
                                &until,
                            )
                            .await;
                            console_error!(
                                "[Facebook Spam Block] Meta error 368: policy/spam block. Pausing until {}.",
                                until
                            );
                            let _ = facebook::send_debounced_fb_admin_alert(
                                db_ref,
                                session_ref,
                                secrets_ref,
                                "spam_block",
                                &format!(
                                    "🚨 AIUB Notice Facebook: Meta blocked post for policy/spam (error 368). Dispatch paused until {until}. Please inspect Facebook Page!"
                                ),
                            )
                            .await;
                            outcome.failed_count += 1;
                            break;
                        }
                        Ok(facebook::FacebookPostResult::Duplicate) => {
                            console_warn!(
                                "[Facebook Duplicate] Error 506 duplicate post for {}; marking handled.",
                                item.notice.url
                            );
                            // O1: accumulate the duplicate-handled mark.
                            // M4: log-and-continue — a failed mark must
                            // not abort the remaining dispatch.
                            let now = Utc::now().to_rfc3339();
                            match db::build_fb_duplicate_handled_statement(
                                db_ref,
                                &item.notice.url,
                                &item.content_hash,
                                &now,
                            ) {
                                Ok(stmt) => outcome.duplicate_stmts.push(stmt),
                                Err(err) => {
                                    console_error!(
                                        "[Facebook Duplicate Mark Failed] Could not build duplicate-handled mark for {}: {}; it will be retried on the next run.",
                                        item.notice.url,
                                        err
                                    );
                                    outcome.failed_count += 1;
                                }
                            }
                            let _ = facebook::send_debounced_fb_admin_alert(
                                db_ref,
                                session_ref,
                                secrets_ref,
                                "duplicate_post",
                                &format!(
                                    "⚠️ AIUB Notice Facebook: Consecutive duplicate rejected by Meta (error 506): {}",
                                    item.notice.url
                                ),
                            )
                            .await;
                        }
                        Ok(facebook::FacebookPostResult::AuthError { message }) => {
                            console_error!("[Facebook Auth Error] {}", message);
                            // H2: store the pause with a timestamp so
                            // is_facebook_paused() can expire it after
                            // FACEBOOK_AUTH_PAUSE_TTL_HOURS.
                            let pause_value = facebook::format_auth_pause_value(&message);
                            let _ = db::set_app_state(
                                db_ref,
                                session_ref,
                                FACEBOOK_AUTH_PAUSED_KEY,
                                &pause_value,
                            )
                            .await;
                            let _ = telegram::send_admin_alert(
                                secrets_ref,
                                &format!("🚨 AIUB Notice Facebook: Authentication paused: {message}"),
                            )
                            .await;
                            outcome.failed_count += 1;
                            break;
                        }
                        Ok(facebook::FacebookPostResult::TransientFailure { message }) => {
                            console_warn!(
                                "[Facebook Transient] Error: {}; will retry at next cron.",
                                message
                            );
                            outcome.deferred_count += 1;
                            break;
                        }
                        Ok(facebook::FacebookPostResult::UnknownError { message }) => {
                            console_error!("[Facebook Unknown Error] {}", message);
                            // M10: an unclassified Meta error previously
                            // got no admin alert, no pause and no
                            // cooldown — it failed silently every run
                            // until someone noticed the logs. Route it
                            // through the existing debounced FB admin
                            // alert (6h, category "unknown_error") so
                            // it is seen once without spamming. No
                            // cooldown/pause is set: the error is
                            // unclassified, so pausing could suppress
                            // good posts for a transient Meta glitch.
                            let _ = facebook::send_debounced_fb_admin_alert(
                                db_ref,
                                session_ref,
                                secrets_ref,
                                "unknown_error",
                                &format!(
                                    "⚠️ AIUB Notice Facebook: Unclassified Meta error, dispatch paused for this run (no cooldown set): {message}"
                                ),
                            )
                            .await;
                            outcome.failed_count += 1;
                            break;
                        }
                        Err(err) => {
                            console_error!(
                                "[Facebook Worker Error] Failed to publish notice {}: {}",
                                item.notice.url,
                                err
                            );
                            outcome.failed_count += 1;
                            break;
                        }
                    }
                }

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
                Ok::<DispatchOutcome, worker::Error>(outcome)
            })
            .await
        };

        // O3: run both phases concurrently, then flush the collected
        // marks sequentially (O1) — Telegram first, then Facebook.
        let (tg_result, fb_result) = futures_util::future::join(tg_future, fb_future).await;
        let tg_outcome = match tg_result {
            Ok(outcome) => outcome,
            Err(err) => {
                console_error!("[Telegram Dispatch Failed] {err}");
                DispatchOutcome::default()
            }
        };
        let fb_outcome = match fb_result {
            Ok(outcome) => outcome,
            Err(err) => {
                console_error!("[Facebook Dispatch Failed] {err}");
                DispatchOutcome::default()
            }
        };
        let tg_flush = flush_dispatch_marks(
            &session,
            "Telegram",
            tg_outcome.attempt_stmts,
            tg_outcome.notified_stmts,
            tg_outcome.duplicate_stmts,
        )
        .await;
        let fb_flush = flush_dispatch_marks(
            &session,
            "Facebook",
            fb_outcome.attempt_stmts,
            fb_outcome.notified_stmts,
            fb_outcome.duplicate_stmts,
        )
        .await;
        total_d1.rows_read += tg_flush.rows_read + fb_flush.rows_read;
        total_d1.rows_written += tg_flush.rows_written + fb_flush.rows_written;
        total_d1.failed_statements += tg_flush.failed_statements + fb_flush.failed_statements;
    }

    let dispatch_ms = elapsed_ms_since(phase_started);

    console_log!(
        "[Run Summary] rows_read={}, rows_written={}, failed_statements={}, silent_backfills={}, detail_succeeded={}, detail_http_errors={}, detail_parse_errors={}, fetch_ms={}, load_ms={}, detail_ms={}, upsert_ms={}, dispatch_ms={}, total_ms={}",
        total_d1.rows_read,
        total_d1.rows_written,
        total_d1.failed_statements,
        silent_backfills,
        detail_succeeded_count,
        detail_http_error_count,
        detail_parse_error_count,
        fetch_listing_ms,
        load_existing_ms,
        detail_fetch_ms,
        upsert_ms,
        dispatch_ms,
        elapsed_ms_since(run_started)
    );

    Ok(())
}

/// M5: sends the detail-failure admin alert at most once per
/// `DETAIL_FAILURE_ALERT_DEBOUNCE_HOURS`, reusing `is_debounce_expired`.
/// The timestamp is only recorded when the alert was actually delivered
/// (or no admin chat is configured).
async fn send_debounced_detail_failure_alert(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    message: &str,
) -> worker::Result<()> {
    let now = Utc::now();
    if let Some(last_alert_str) = db::get_app_state(session, DETAIL_FAILURE_LAST_ALERT_KEY).await? {
        if !is_debounce_expired(
            Some(&last_alert_str),
            now,
            DETAIL_FAILURE_ALERT_DEBOUNCE_HOURS,
        ) {
            console_log!(
                "Suppressing duplicate detail-failure alert (last sent at {})",
                last_alert_str
            );
            return Ok(());
        }
    }

    let alert_sent = match telegram::send_admin_alert(secrets, message).await {
        Ok(true) => true,
        Ok(false) => secrets.telegram_admin_chat_id.is_none(),
        Err(err) => {
            console_warn!(
                "[Detail Failure Alert Failed] Failed to send detail-failure alert: {err}; will retry on the next run."
            );
            false
        }
    };

    if alert_sent {
        let now_str = now.to_rfc3339();
        db::set_app_state(db, session, DETAIL_FAILURE_LAST_ALERT_KEY, &now_str).await?;
    }
    Ok(())
}

/// M15: result of the pure detail-target selection.
pub(crate) struct DetailTargetSelection {
    pub targets: Vec<String>,
    pub new_count: usize,
    pub backfill_count: usize,
    pub rotation_count: usize,
}

/// M15: pure detail-target selection (Priority A/B/C), extracted from
/// `run()` so the orchestration decisions are unit-testable instead of
/// only reachable through a live D1 + AIUB run.
///
/// - Priority A: brand-new notices not in D1 yet.
/// - Priority B: existing notices where `detail_fetched_at` is NULL
///   (respecting the retry cooldown), rows containing Latin-1 mojibake
///   that need healing, OR notices whose listing title/description
///   changed on page 1.
/// - Priority C: existing notices ordered by oldest `detail_checked_at`
///   (rolling edit check).
pub(crate) fn select_detail_targets(
    unique_listing: &[Notice],
    existing_map: &HashMap<String, NoticeRow>,
    retry_cutoff_iso: &str,
) -> DetailTargetSelection {
    let mut selected: Vec<String> = Vec::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);
    let mut selected_set: HashSet<String> = HashSet::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);

    // Priority A: Brand-new notices
    let mut new_count = 0usize;
    for item in unique_listing {
        if selected.len() >= MAX_DETAIL_FETCHES_PER_RUN {
            break;
        }
        if !existing_map.contains_key(&item.url) && selected_set.insert(item.url.clone()) {
            selected.push(item.url.clone());
            new_count += 1;
        }
    }

    // Priority B: Unfetched detail pages, mojibake repair, OR listing changes
    let mut backfill_count = 0usize;
    for item in unique_listing {
        if selected.len() >= MAX_DETAIL_FETCHES_PER_RUN {
            break;
        }
        if let Some(row) = existing_map.get(&item.url) {
            let row_has_mojibake = has_utf8_mojibake(&row.title)
                || has_utf8_mojibake(&row.description)
                || has_utf8_mojibake(&row.body_text);
            let listing_changed = !titles_equivalent_clean(&item.title, &row.title)
                || !clean_mojibake_eq(&row.description, &item.description);
            let needs_backfill =
                row.detail_fetched_at.is_none() && row.detail_due(retry_cutoff_iso);
            // M7: mojibake / listing-change backfill is gated on the
            // detail retry cooldown too. Previously it bypassed the
            // 30-minute cooldown entirely, so a persistently mojibake or
            // changed row was re-fetched on every 5-minute run.
            let detail_due = row.detail_due(retry_cutoff_iso);

            if (needs_backfill || ((row_has_mojibake || listing_changed) && detail_due))
                && selected_set.insert(item.url.clone())
            {
                selected.push(item.url.clone());
                backfill_count += 1;
            }
        }
    }

    // Priority C: Rolling edit check across visible notices (oldest detail_checked_at first)
    let mut rotation_count = 0usize;
    if selected.len() < MAX_DETAIL_FETCHES_PER_RUN {
        let mut candidates: Vec<&NoticeRow> = existing_map
            .values()
            .filter(|r| r.detail_fetched_at.is_some() && !selected_set.contains(&r.link))
            .collect();

        candidates.sort_by(|a, b| {
            let a_ts = a.detail_checked_at.as_deref().unwrap_or("");
            let b_ts = b.detail_checked_at.as_deref().unwrap_or("");
            a_ts.cmp(b_ts)
        });

        for row in candidates {
            if selected.len() >= MAX_DETAIL_FETCHES_PER_RUN {
                break;
            }
            if selected_set.insert(row.link.clone()) {
                selected.push(row.link.clone());
                rotation_count += 1;
            }
        }
    }

    DetailTargetSelection {
        targets: selected,
        new_count,
        backfill_count,
        rotation_count,
    }
}

/// M15: per-platform notification eligibility, shared by the Telegram and
/// Facebook dispatch phases (their logic was duplicated inline in
/// `run()`). Returns `Some(is_edited)` when the notice should be queued,
/// `None` otherwise.
pub(crate) fn platform_notification(
    sent_at: Option<&str>,
    last_notified_hash: Option<&str>,
    detail_fetched: bool,
    detail_attempted: bool,
    detail_checked: bool,
    new_hash: &str,
) -> Option<bool> {
    match sent_at {
        None => {
            if detail_fetched || detail_attempted || detail_checked {
                Some(false)
            } else {
                None
            }
        }
        Some(_) => {
            if detail_attempted && last_notified_hash != Some(new_hash) {
                Some(true)
            } else {
                None
            }
        }
    }
}

/// M15: silent-sync eligibility for one existing row, extracted from
/// `run()`. Returns `(tg_needs_sync, fb_needs_sync)` — each true only
/// when that platform has already sent this notice (`sent_at` is set)
/// and its stored hash differs, and the change is a non-edit backfill:
/// first-time detail fetch, metadata/mojibake healing, or legacy sticky
/// header re-baseline.
///
/// H1 (preserved): a platform that has never sent must never be marked
/// sent by a silent sync — only its hash is synced, and only when it
/// has already sent.
///
/// O5: `attachments_json` is computed once by the caller (`run()`),
/// not re-serialized here.
pub(crate) fn silent_sync_flags(
    row: &NoticeRow,
    listing_title: &str,
    item: &Notice,
    new_hash: &str,
    attachments_json: &str,
    matched_legacy_sticky: bool,
) -> (bool, bool) {
    let tg_needs_sync =
        row.tg_sent_at.is_some() && row.tg_last_notified_hash.as_deref() != Some(new_hash);
    let fb_needs_sync =
        row.fb_sent_at.is_some() && row.fb_last_notified_hash.as_deref() != Some(new_hash);

    if !tg_needs_sync && !fb_needs_sync {
        return (false, false);
    }

    // M6: compare the listing title only. The old check also required
    // `item.title` — which by this point may be the full detail title —
    // to be equivalent to the cached title, so a truncated/long title
    // defeated the sync and produced a false `[EDITED]` notification.
    let listing_unchanged = titles_equivalent_clean(listing_title, &row.title)
        && clean_mojibake_eq(&row.description, &item.description);

    let first_detail_backfill =
        row.detail_fetched_at.is_none() && item.detail_fetched && listing_unchanged;

    let metadata_or_mojibake_backfill = listing_unchanged
        && clean_mojibake_eq(&row.body_text, &item.body_text)
        && row.attachments_json == attachments_json;

    let sticky_header_rebaseline =
        matched_legacy_sticky && item.detail_fetched && listing_unchanged;

    if first_detail_backfill || metadata_or_mojibake_backfill || sticky_header_rebaseline {
        (tg_needs_sync, fb_needs_sync)
    } else {
        (false, false)
    }
}

/// Detects whether a string contains Latin-1 mojibake (`\u{00e2}\u{0080}` or `\u{00e2}\u{0086}`)
/// produced by the old `bytes[i] as char` loop.
fn has_utf8_mojibake(s: &str) -> bool {
    s.contains("\u{00e2}\u{0080}") || s.contains("\u{00e2}\u{0086}")
}

/// Strips both real Unicode punctuation and 3-char Latin-1 mojibake sequences (`\u{00e2}\u{0080}_`)
/// so corrupted D1 strings compare equal to clean UTF-8 strings during silent healing.
///
/// L6: keeps Unicode alphanumerics (`is_alphanumeric`, not ASCII-only), so
/// a notice whose only change is in Bengali / accented text is still
/// detected as an edit instead of comparing equal.
fn clean_mojibake(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{00e2}' {
            if let Some(&next_c) = chars.peek() {
                if next_c == '\u{0080}' || next_c == '\u{0086}' {
                    let _ = chars.next();
                    let _ = chars.next();
                    continue;
                }
            }
        }
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Efficiently checks if two strings are equivalent after stripping mojibake and non-alphanumeric chars,
/// without allocating any new heap Strings when no mojibake is present.
/// Uses Unicode `is_alphanumeric` (L6) so Bengali/accented-only edits count.
fn clean_mojibake_eq(a: &str, b: &str) -> bool {
    if !has_utf8_mojibake(a) && !has_utf8_mojibake(b) {
        let it_a = a
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase());
        let it_b = b
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase());
        return it_a.eq(it_b);
    }
    clean_mojibake(a) == clean_mojibake(b)
}

fn titles_equivalent_clean(listing_title: &str, cached_title: &str) -> bool {
    if clean_mojibake_eq(listing_title, cached_title) {
        return true;
    }
    titles_equivalent(listing_title, cached_title)
}
