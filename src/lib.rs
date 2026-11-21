use chrono::{DateTime, Duration, Utc};
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
use worker::{
    console_error, console_log, console_warn,
    d1::{D1Database, D1DatabaseSession},
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
    DETAIL_FAILURE_MIN_SAMPLE, DETAIL_FAILURE_RATIO, DETAIL_RETRY_COOLDOWN_MINUTES,
    FACEBOOK_SPAM_BLOCK_COOLDOWN_HOURS, MAX_DETAIL_FETCHES_PER_RUN, MAX_FACEBOOK_SENDS_PER_RUN,
    MAX_TELEGRAM_SENDS_PER_RUN, MIN_LISTING_DATE_COVERAGE, MIN_LISTING_DESC_COVERAGE,
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
    let down_since_str = db::get_app_state(session, AIUB_OUTAGE_DOWN_SINCE_KEY).await?;
    let down_since = match down_since_str {
        Some(ref s) if !s.is_empty() => match DateTime::parse_from_rfc3339(s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                let now_str = now.to_rfc3339();
                let _ = db::set_app_state(db, session, AIUB_OUTAGE_DOWN_SINCE_KEY, &now_str).await;
                now
            }
        },
        _ => {
            let now_str = now.to_rfc3339();
            let _ = db::set_app_state(db, session, AIUB_OUTAGE_DOWN_SINCE_KEY, &now_str).await;
            now
        }
    };

    let elapsed = now.signed_duration_since(down_since);
    if elapsed.num_minutes() >= AIUB_OUTAGE_ALERT_THRESHOLD_MINS {
        let already_alerted = db::get_app_state(session, AIUB_OUTAGE_ALERTED_KEY)
            .await?
            .as_deref()
            == Some("true");

        if !already_alerted {
            let duration_str = format_duration_human(elapsed);
            let alert_msg = format_outage_alert_message(&duration_str);
            let alert_sent = telegram::send_admin_alert(secrets, &alert_msg)
                .await
                .unwrap_or(false);
            if alert_sent || secrets.telegram_admin_chat_id.is_none() {
                let _ = db::set_app_state(db, session, AIUB_OUTAGE_ALERTED_KEY, "true").await;
            }
        }
    }

    Ok(())
}

async fn handle_upstream_recovery(
    db: &D1Database,
    session: &D1DatabaseSession,
    secrets: &Secrets,
    now: DateTime<Utc>,
) -> worker::Result<()> {
    let down_since_str = db::get_app_state(session, AIUB_OUTAGE_DOWN_SINCE_KEY).await?;
    let Some(down_since_str) = down_since_str else {
        return Ok(());
    };
    if down_since_str.is_empty() {
        return Ok(());
    }

    let already_alerted = db::get_app_state(session, AIUB_OUTAGE_ALERTED_KEY)
        .await?
        .as_deref()
        == Some("true");

    if already_alerted {
        if let Ok(down_since) = DateTime::parse_from_rfc3339(&down_since_str) {
            let total_duration = now.signed_duration_since(down_since.with_timezone(&Utc));
            let duration_str = format_duration_human(total_duration);
            let recovery_msg = format_recovery_alert_message(&duration_str);
            let _ = telegram::send_admin_alert(secrets, &recovery_msg).await;
        }
    }

    let _ = db::delete_app_state_keys(
        db,
        session,
        &[AIUB_OUTAGE_DOWN_SINCE_KEY, AIUB_OUTAGE_ALERTED_KEY],
    )
    .await;

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

async fn run(env: &Env) -> worker::Result<()> {
    let secrets = Secrets::from_env(env)?;
    let database = env.d1("DB")?;

    // Start a sequentially consistent D1 session anchored to the primary instance.
    let session = database.with_session(Some("first-primary"))?;
    let mut total_d1 = D1Metrics::default();

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

    if let Err(err) = handle_upstream_recovery(&database, &session, &secrets, Utc::now()).await {
        console_warn!("[Recovery State Warning] Failed to handle outage recovery: {err}");
    }

    if listing_notices.is_empty() {
        console_warn!(
            "[Listing Parse Failed] 0 notices parsed from listing page 1. DOM layout may have changed."
        );
        let _ = telegram::send_admin_alert(
            &secrets,
            "⚠️ AIUB Notice Checker: 0 notices found on listing page 1. DOM layout may have changed.",
        )
        .await;
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

    // 3. Select up to MAX_DETAIL_FETCHES_PER_RUN (5) detail pages strictly from the visible notices:
    //    Priority A: Brand-new notices not in D1 yet.
    //    Priority B: Existing notices where detail_fetched_at IS NULL (respecting retry cooldown),
    //                rows containing Latin-1 mojibake (`â`) that need healing, OR notices whose
    //                listing title/description changed on page 1.
    //    Priority C: Existing notices ordered by oldest detail_checked_at (rolling edit check).
    let retry_cutoff_iso =
        (Utc::now() - Duration::minutes(DETAIL_RETRY_COOLDOWN_MINUTES)).to_rfc3339();

    let detail_targets = enter_span("select_detail_targets", |span| {
        let mut selected: Vec<String> = Vec::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);
        let mut selected_set: HashSet<String> = HashSet::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);

        // Priority A: Brand-new notices
        let mut new_count = 0i64;
        for item in &unique_listing {
            if selected.len() >= MAX_DETAIL_FETCHES_PER_RUN {
                break;
            }
            if !existing_map.contains_key(&item.url) && selected_set.insert(item.url.clone()) {
                selected.push(item.url.clone());
                new_count += 1;
            }
        }

        // Priority B: Unfetched detail pages, mojibake repair, OR listing changes
        let mut backfill_count = 0i64;
        for item in &unique_listing {
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
                    row.detail_fetched_at.is_none() && row.detail_due(&retry_cutoff_iso);

                if (needs_backfill || row_has_mojibake || listing_changed)
                    && selected_set.insert(item.url.clone())
                {
                    selected.push(item.url.clone());
                    backfill_count += 1;
                }
            }
        }

        // Priority C: Rolling edit check across visible notices (oldest detail_checked_at first)
        let mut rotation_count = 0i64;
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

        span.set_attribute("new_targets", new_count);
        span.set_attribute("backfill_targets", backfill_count);
        span.set_attribute("rotation_targets", rotation_count);
        span.set_attribute("total_targets", selected.len() as i64);
        span.set_status(SpanStatus::Ok);
        selected
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

        let _ = telegram::send_admin_alert(&secrets, &alert_message).await;
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
        is_edited: bool,
    }

    let mut statements = Vec::new();
    let mut tg_notifications: Vec<PendingNotification> = Vec::new();
    let mut fb_notifications: Vec<PendingNotification> = Vec::new();
    let mut silent_sync_links: Vec<(String, String)> = Vec::new();

    // Cold-start guard: if D1 is completely empty and we scraped a full page of notices,
    // this is a fresh database seed — write all rows silently and populate tracking columns
    // so neither Telegram nor Facebook are flooded.
    let is_empty_db_seed = existing_map.is_empty() && unique_listing.len() >= 10;
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

        // Silent hash sync for non-edit backfills:
        // 1. First-time detail fetch (`row.detail_fetched_at.is_none()`) when listing fields didn't change.
        // 2. Date-only, full-title, or UTF-8 mojibake healing when content is equivalent.
        // 3. Re-baselining rows from version `e1a14563` whose stored `body_text` was the sticky header.
        let mut silent_hash_sync = false;
        if let Some(row) = existing {
            let tg_needs_sync = row.tg_sent_at.is_some()
                && row.tg_last_notified_hash.as_deref() != Some(new_hash.as_str());
            let fb_needs_sync = row.fb_sent_at.is_some()
                && row.fb_last_notified_hash.as_deref() != Some(new_hash.as_str());

            if tg_needs_sync || fb_needs_sync {
                let listing_unchanged = titles_equivalent_clean(&listing_title, &row.title)
                    && titles_equivalent_clean(&item.title, &row.title)
                    && clean_mojibake_eq(&row.description, &item.description);

                let first_detail_backfill =
                    row.detail_fetched_at.is_none() && item.detail_fetched && listing_unchanged;

                let metadata_or_mojibake_backfill = listing_unchanged
                    && clean_mojibake_eq(&row.body_text, &item.body_text)
                    && row.attachments_json == item.attachments_json();

                let sticky_header_rebaseline =
                    matched_legacy_sticky && item.detail_fetched && listing_unchanged;

                if first_detail_backfill
                    || metadata_or_mojibake_backfill
                    || sticky_header_rebaseline
                {
                    silent_hash_sync = true;
                }
            }
        }

        if needs_db_write {
            if use_lightweight_update {
                let stmt =
                    db::build_lightweight_detail_update(&database, &item.url, item.detail_fetched)?;
                statements.push(stmt);
            } else {
                let stmt =
                    db::build_upsert_statement(&database, &item, &new_hash, detail_attempted)?;
                statements.push(stmt);
            }
        }

        if silent_hash_sync {
            silent_sync_links.push((item.url.clone(), new_hash));
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
                    tg_notifications.push(PendingNotification {
                        notice: item.clone(),
                        content_hash: new_hash.clone(),
                        is_edited: false,
                    });
                    fb_notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        is_edited: false,
                    });
                }
            }
            Some(row) => {
                // Telegram notification eligibility
                if row.tg_sent_at.is_none() {
                    if item.detail_fetched || detail_attempted || row.detail_checked_at.is_some() {
                        tg_notifications.push(PendingNotification {
                            notice: item.clone(),
                            content_hash: new_hash.clone(),
                            is_edited: false,
                        });
                    }
                } else if detail_attempted
                    && row.tg_last_notified_hash.as_deref() != Some(new_hash.as_str())
                {
                    tg_notifications.push(PendingNotification {
                        notice: item.clone(),
                        content_hash: new_hash.clone(),
                        is_edited: true,
                    });
                }

                // Facebook notification eligibility (fully independent tracking)
                if row.fb_sent_at.is_none() {
                    if item.detail_fetched || detail_attempted || row.detail_checked_at.is_some() {
                        fb_notifications.push(PendingNotification {
                            notice: item,
                            content_hash: new_hash,
                            is_edited: false,
                        });
                    }
                } else if detail_attempted
                    && row.fb_last_notified_hash.as_deref() != Some(new_hash.as_str())
                {
                    fb_notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        is_edited: true,
                    });
                }
            }
        }
    }

    let silent_backfills = silent_sync_links.len();

    // 6. Execute D1 batch upserts on the session.
    if !statements.is_empty() {
        let stmt_count = statements.len();
        let batch_metrics = enter_span_async("d1_batch_upsert", |span| async move {
            span.set_attribute("statements", stmt_count as i64);
            match db::execute_batch(session_ref, statements).await {
                Ok(m) => {
                    span.set_attribute("rows_read", m.rows_read as i64);
                    span.set_attribute("rows_written", m.rows_written as i64);
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

        total_d1.rows_read += batch_metrics.rows_read;
        total_d1.rows_written += batch_metrics.rows_written;
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
    if !silent_sync_links.is_empty() {
        let now = Utc::now().to_rfc3339();
        let mut sync_stmts = Vec::with_capacity(silent_sync_links.len() * 2);
        for (url, hash) in &silent_sync_links {
            sync_stmts.push(db::build_tg_notified_statement(&database, url, hash, &now)?);
            sync_stmts.push(db::build_fb_hash_sync_statement(&database, url, hash)?);
        }
        let sync_metrics = db::execute_batch(&session, sync_stmts).await?;
        total_d1.rows_read += sync_metrics.rows_read;
        total_d1.rows_written += sync_metrics.rows_written;
    }

    // 7. Dispatch queued Telegram notifications (oldest-first).
    if !tg_notifications.is_empty() {
        tg_notifications.reverse();
        let db_ref = &database;
        let secrets_ref = &secrets;

        let notify_metrics =
            enter_span_async("dispatch_telegram_notifications", |span| async move {
                span.set_attribute("queued", tg_notifications.len() as i64);
                let mut sent_count = 0usize;
                let mut deferred_count = 0usize;
                let mut failed_count = 0usize;
                let mut metrics = D1Metrics::default();

                for item in tg_notifications.into_iter().take(MAX_TELEGRAM_SENDS_PER_RUN) {
                    if is_near_cpu_limit() {
                        console_warn!(
                            "[Telegram Deferred] Near CPU limit during Telegram dispatch; deferring remaining."
                        );
                        deferred_count += 1;
                        break;
                    }

                    if let Ok(m) =
                        db::mark_tg_notification_attempt(db_ref, session_ref, &item.notice.url).await
                    {
                        metrics.rows_read += m.rows_read;
                        metrics.rows_written += m.rows_written;
                    }

                    match telegram::send_notice_alert(secrets_ref, &item.notice, item.is_edited)
                        .await
                    {
                        Ok(true) => {
                            let m = db::mark_tg_notified(
                                db_ref,
                                session_ref,
                                &item.notice.url,
                                &item.content_hash,
                            )
                            .await?;
                            metrics.rows_read += m.rows_read;
                            metrics.rows_written += m.rows_written;
                            sent_count += 1;
                        }
                        Ok(false) => {
                            console_warn!(
                                "[Telegram Deferred] Rate limit hit for notice {}; deferring to next run.",
                                item.notice.url
                            );
                            deferred_count += 1;
                            break;
                        }
                        Err(err) => {
                            console_error!(
                                "[Telegram Send Failed] Telegram send failed for {}: {}",
                                item.notice.url,
                                err
                            );
                            failed_count += 1;
                        }
                    }
                }

                span.set_attribute("sent", sent_count as i64);
                span.set_attribute("deferred", deferred_count as i64);
                span.set_attribute("failed", failed_count as i64);
                if failed_count > 0 {
                    span.set_status(SpanStatus::Error(Some(format!("{failed_count} sends failed"))));
                } else {
                    span.set_status(SpanStatus::Ok);
                }
                Ok::<D1Metrics, worker::Error>(metrics)
            })
            .await?;

        total_d1.rows_read += notify_metrics.rows_read;
        total_d1.rows_written += notify_metrics.rows_written;
    }

    // 8. Dispatch queued Facebook notifications (oldest-first, independent phase).
    if let Some(fb_secrets) = &secrets.facebook {
        if !fb_notifications.is_empty() {
            fb_notifications.reverse();
            let db_ref = &database;
            let secrets_ref = &secrets;
            let fb_secrets_ref = fb_secrets;

            let fb_metrics =
                enter_span_async("dispatch_facebook_notifications", |span| async move {
                    span.set_attribute("queued", fb_notifications.len() as i64);

                    let (paused, pause_reason) =
                        facebook::is_facebook_paused(db_ref, session_ref).await?;
                    if paused {
                        console_log!(
                            "[Facebook Dispatch Skipped] {}",
                            pause_reason.as_deref().unwrap_or("paused")
                        );
                        span.set_attribute(
                            "skipped_reason",
                            pause_reason.unwrap_or_else(|| "paused".to_string()),
                        );
                        span.set_status(SpanStatus::Ok);
                        return Ok::<D1Metrics, worker::Error>(D1Metrics::default());
                    }

                    let mut sent_count = 0usize;
                    let mut deferred_count = 0usize;
                    let mut failed_count = 0usize;
                    let mut metrics = D1Metrics::default();

                    for item in fb_notifications.into_iter().take(MAX_FACEBOOK_SENDS_PER_RUN) {
                        if is_near_cpu_limit() {
                            console_warn!(
                                "[Facebook Deferred] Near CPU limit during Facebook dispatch; deferring remaining."
                            );
                            deferred_count += 1;
                            break;
                        }

                        if let Ok(m) =
                            db::mark_fb_notification_attempt(db_ref, session_ref, &item.notice.url)
                                .await
                        {
                            metrics.rows_read += m.rows_read;
                            metrics.rows_written += m.rows_written;
                        }

                        let message = format_notice_message(&item.notice, item.is_edited);
                        let post_outcome = facebook::publish_post(fb_secrets_ref, &message).await;

                        match post_outcome {
                            Ok(facebook::FacebookPostResult::Success { post_id, usage }) => {
                                let m = db::mark_fb_notified(
                                    db_ref,
                                    session_ref,
                                    &item.notice.url,
                                    &item.content_hash,
                                    &post_id,
                                )
                                .await?;
                                metrics.rows_read += m.rows_read;
                                metrics.rows_written += m.rows_written;
                                sent_count += 1;

                                if let Some(u) = usage {
                                    if u.is_approaching_limit() {
                                        let until = (Utc::now() + Duration::minutes(15)).to_rfc3339();
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
                                let until = (Utc::now() + Duration::minutes(cooldown_minutes)).to_rfc3339();
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
                                deferred_count += 1;
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
                                failed_count += 1;
                                break;
                            }
                            Ok(facebook::FacebookPostResult::Duplicate) => {
                                console_warn!(
                                    "[Facebook Duplicate] Error 506 duplicate post for {}; marking handled.",
                                    item.notice.url
                                );
                                let m = db::mark_fb_duplicate_handled(
                                    db_ref,
                                    session_ref,
                                    &item.notice.url,
                                    &item.content_hash,
                                )
                                .await?;
                                metrics.rows_read += m.rows_read;
                                metrics.rows_written += m.rows_written;
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
                                let _ = db::set_app_state(
                                    db_ref,
                                    session_ref,
                                    "fb_auth_paused",
                                    &message,
                                )
                                .await;
                                let _ = telegram::send_admin_alert(
                                    secrets_ref,
                                    &format!("🚨 AIUB Notice Facebook: Authentication paused: {message}"),
                                )
                                .await;
                                failed_count += 1;
                                break;
                            }
                            Ok(facebook::FacebookPostResult::TransientFailure { message }) => {
                                console_warn!(
                                    "[Facebook Transient] Error: {}; will retry at next cron.",
                                    message
                                );
                                deferred_count += 1;
                                break;
                            }
                            Ok(facebook::FacebookPostResult::UnknownError { message }) => {
                                console_error!("[Facebook Unknown Error] {}", message);
                                failed_count += 1;
                                break;
                            }
                            Err(err) => {
                                console_error!(
                                    "[Facebook Worker Error] Failed to publish notice {}: {}",
                                    item.notice.url,
                                    err
                                );
                                failed_count += 1;
                                break;
                            }
                        }
                    }

                    span.set_attribute("sent", sent_count as i64);
                    span.set_attribute("deferred", deferred_count as i64);
                    span.set_attribute("failed", failed_count as i64);
                    if failed_count > 0 {
                        span.set_status(SpanStatus::Error(Some(format!("{failed_count} sends failed"))));
                    } else {
                        span.set_status(SpanStatus::Ok);
                    }
                    Ok::<D1Metrics, worker::Error>(metrics)
                })
                .await?;

            total_d1.rows_read += fb_metrics.rows_read;
            total_d1.rows_written += fb_metrics.rows_written;
        }
    }

    console_log!(
        "[Run Summary] rows_read={}, rows_written={}, silent_backfills={}, detail_succeeded={}, detail_http_errors={}, detail_parse_errors={}",
        total_d1.rows_read,
        total_d1.rows_written,
        silent_backfills,
        detail_succeeded_count,
        detail_http_error_count,
        detail_parse_error_count
    );

    Ok(())
}

/// Detects whether a string contains Latin-1 mojibake (`\u{00e2}\u{0080}` or `\u{00e2}\u{0086}`)
/// produced by the old `bytes[i] as char` loop.
fn has_utf8_mojibake(s: &str) -> bool {
    s.contains("\u{00e2}\u{0080}") || s.contains("\u{00e2}\u{0086}")
}

/// Strips both real Unicode punctuation and 3-char Latin-1 mojibake sequences (`\u{00e2}\u{0080}_`)
/// so corrupted D1 strings compare equal to clean UTF-8 strings during silent healing.
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
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// Efficiently checks if two strings are equivalent after stripping mojibake and non-alphanumeric chars,
/// without allocating any new heap Strings when no mojibake is present.
fn clean_mojibake_eq(a: &str, b: &str) -> bool {
    if !has_utf8_mojibake(a) && !has_utf8_mojibake(b) {
        let it_a = a
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(|c| c.to_lowercase());
        let it_b = b
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
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
