use chrono::{Duration, Utc};
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, HashSet};
use worker::{
    console_error, console_log, console_warn,
    event,
    observability::{enter_span, enter_span_async, SpanStatus},
    signal::is_near_cpu_limit,
    Env, ScheduleContext, ScheduledEvent,
};

mod config;
mod db;
mod models;
mod scraper;
mod telegram;

use config::{
    Secrets, DETAIL_FAILURE_MIN_SAMPLE, DETAIL_FAILURE_RATIO, DETAIL_RETRY_COOLDOWN_MINUTES,
    MAX_DETAIL_FETCHES_PER_RUN, MAX_TELEGRAM_SENDS_PER_RUN, MIN_LISTING_DATE_COVERAGE,
    MIN_LISTING_DESC_COVERAGE,
};
use models::{titles_equivalent, D1Metrics, Notice, NoticeRow};

const DETAIL_CONCURRENCY: usize = 3;

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

    // 1. Fetch the 100 latest notices on listing page 1.
    let listing_notices: Vec<Notice> = enter_span_async("fetch_listing", |span| async move {
        let listing_url = scraper::listing_page_url(1);
        span.set_attribute("url", listing_url.as_str());
        let html = match scraper::fetch_html(&listing_url).await {
            Ok(h) => h,
            Err(err) => {
                span.set_status(SpanStatus::Error(Some(err.to_string())));
                return Err(err);
            }
        };
        let parsed = scraper::parse_listing_html(&html);
        span.set_attribute("notices_found", parsed.len() as i64);
        span.set_status(SpanStatus::Ok);
        Ok::<Vec<Notice>, worker::Error>(parsed)
    })
    .await?;

    if listing_notices.is_empty() {
        console_warn!("No notices parsed on listing page 1; alerting admin if configured.");
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

    // Listing coverage sanity check (warns if date or description selectors degrade).
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

    // 2. Load existing D1 rows strictly for the current 100 notices.
    let session_ref = &session;
    let links_ref = &links;
    let (existing_map, read_metrics) =
        enter_span_async("load_existing_100_from_d1", |span| async move {
            span.set_attribute("queried_links", links_ref.len() as i64);
            match db::get_notices_by_links(session_ref, links_ref).await {
                Ok((map, m)) => {
                    span.set_attribute("matched_rows", map.len() as i64);
                    span.set_attribute("rows_read", m.rows_read as i64);
                    span.set_status(SpanStatus::Ok);
                    Ok::<(HashMap<String, NoticeRow>, D1Metrics), worker::Error>((map, m))
                }
                Err(err) => {
                    span.set_status(SpanStatus::Error(Some(err.to_string())));
                    Err(err)
                }
            }
        })
        .await?;

    total_d1.rows_read += read_metrics.rows_read;
    total_d1.rows_written += read_metrics.rows_written;

    // 3. Select up to MAX_DETAIL_FETCHES_PER_RUN (5) detail pages strictly from the 100 notices:
    //    Priority A: Brand-new notices not in D1 yet.
    //    Priority B: Existing notices in the 100 where detail_fetched_at IS NULL and retry cooldown elapsed.
    //    Priority C: Existing notices in the 100 ordered by oldest detail_checked_at (rolling edit check).
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

        // Priority B: Unfetched detail pages in the 100 (respecting retry cooldown)
        let mut backfill_count = 0i64;
        for item in &unique_listing {
            if selected.len() >= MAX_DETAIL_FETCHES_PER_RUN {
                break;
            }
            if let Some(row) = existing_map.get(&item.url) {
                if row.detail_fetched_at.is_none()
                    && row.detail_due(&retry_cutoff_iso)
                    && selected_set.insert(item.url.clone())
                {
                    selected.push(item.url.clone());
                    backfill_count += 1;
                }
            }
        }

        // Priority C: Rolling edit check across the 100 notices (oldest detail_checked_at first)
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

        let fetched: Vec<(String, Option<(String, Vec<String>)>)> =
            stream::iter(targets_ref.iter().cloned())
                .map(|link| async move {
                    if is_near_cpu_limit() {
                        console_warn!(
                            "Near CPU limit before fetching detail page {}; skipping.",
                            link
                        );
                        return (link, None);
                    }
                    match scraper::fetch_html(&link).await {
                        Ok(html) => {
                            let parsed = scraper::parse_detail_html(&html, &link);
                            if parsed.is_none() {
                                console_warn!("Detail parse returned empty for {}", link);
                            }
                            (link, parsed)
                        }
                        Err(err) => {
                            console_warn!("Detail HTTP fetch failed for {}: {}", link, err);
                            (link, None)
                        }
                    }
                })
                .buffer_unordered(DETAIL_CONCURRENCY)
                .collect()
                .await;

        let ok_count = fetched.iter().filter(|(_, res)| res.is_some()).count();
        span.set_attribute("succeeded", ok_count as i64);
        span.set_status(SpanStatus::Ok);
        fetched
    })
    .await;

    let attempted_count = detail_results.len();
    let failed_count = detail_results.iter().filter(|(_, r)| r.is_none()).count();
    if attempted_count >= DETAIL_FAILURE_MIN_SAMPLE
        && (failed_count as f64 / attempted_count as f64) >= DETAIL_FAILURE_RATIO
    {
        console_warn!(
            "High detail page failure rate: {}/{} failed",
            failed_count,
            attempted_count
        );
    }

    let mut detail_map: HashMap<String, Option<(String, Vec<String>)>> =
        HashMap::with_capacity(detail_results.len());
    for (link, outcome) in detail_results {
        detail_map.insert(link, outcome);
    }

    // 5. Reconcile state, compute hashes, build D1 batch upserts, and queue notifications.
    struct PendingNotification {
        notice: Notice,
        content_hash: String,
        is_edited: bool,
    }

    let mut statements = Vec::new();
    let mut notifications: Vec<PendingNotification> = Vec::new();
    let mut silent_backfills = 0usize;

    for mut item in unique_listing {
        let existing = existing_map.get(&item.url);

        // Preserve full cached title if the listing title was truncated with "..."
        if let Some(row) = existing {
            if item.title.contains("...") && titles_equivalent(&item.title, &row.title) {
                item.title = row.title.clone();
            }
            if item.published_date.is_none() && row.published_date.is_some() {
                item.published_date = row.published_date.clone();
            }
        }

        let detail_outcome = detail_map.get(&item.url);
        let detail_attempted = detail_outcome.is_some();

        match (detail_outcome, existing) {
            (Some(Some((body, attachments))), _) => {
                item.body_text = body.clone();
                item.attachments = attachments.clone();
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

        let needs_db_write = match existing {
            None => true,
            Some(row) => {
                detail_attempted
                    || row.content_hash != new_hash
                    || row.title != item.title
                    || row.description != item.description
                    || row.published_date != item.published_date
            }
        };

        // Silent backfill check: if a notice was already notified before its detail page was
        // ever fetched, and its listing fields haven't changed, update last_notified_hash
        // silently so the first detail backfill doesn't fire a false [EDITED] alert.
        let mut silent_hash_sync = false;
        if let Some(row) = existing {
            if row.sent_at.is_some()
                && row.detail_fetched_at.is_none()
                && item.detail_fetched
                && titles_equivalent(&item.title, &row.title)
                && row.description == item.description
                && row.published_date == item.published_date
            {
                silent_hash_sync = true;
                silent_backfills += 1;
            }
        }

        if needs_db_write {
            let stmt = db::build_upsert_statement(&database, &item, &new_hash, detail_attempted)?;
            statements.push(stmt);
        }

        if silent_hash_sync {
            let m = db::mark_notified(&database, &session, &item.url, &new_hash).await?;
            total_d1.rows_read += m.rows_read;
            total_d1.rows_written += m.rows_written;
            continue;
        }

        match existing {
            None => {
                // Brand-new notice: send immediately if detail was fetched (or attempted),
                // otherwise wait 5 minutes for the next run's detail budget.
                if item.detail_fetched || detail_attempted {
                    notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        is_edited: false,
                    });
                }
            }
            Some(row) => {
                if row.sent_at.is_none() {
                    if item.detail_fetched || detail_attempted || row.detail_checked_at.is_some() {
                        notifications.push(PendingNotification {
                            notice: item,
                            content_hash: new_hash,
                            is_edited: false,
                        });
                    }
                } else if row.last_notified_hash.as_deref() != Some(new_hash.as_str()) {
                    notifications.push(PendingNotification {
                        notice: item,
                        content_hash: new_hash,
                        is_edited: true,
                    });
                }
            }
        }
    }

    // 6. Execute D1 batch upserts on the session.
    if !statements.is_empty() {
        let stmt_count = statements.len();
        let batch_metrics = enter_span_async("d1_batch_upsert", |span| async move {
            span.set_attribute("statements", stmt_count as i64);
            let m = db::execute_batch(session_ref, statements).await?;
            span.set_attribute("rows_read", m.rows_read as i64);
            span.set_attribute("rows_written", m.rows_written as i64);
            span.set_status(SpanStatus::Ok);
            Ok::<D1Metrics, worker::Error>(m)
        })
        .await?;

        total_d1.rows_read += batch_metrics.rows_read;
        total_d1.rows_written += batch_metrics.rows_written;
    }

    // 7. Dispatch queued Telegram notifications (oldest-first).
    if !notifications.is_empty() {
        notifications.reverse();
        let db_ref = &database;
        let secrets_ref = &secrets;

        let notify_metrics =
            enter_span_async("dispatch_telegram_notifications", |span| async move {
                span.set_attribute("queued", notifications.len() as i64);
                let mut sent_count = 0usize;
                let mut metrics = D1Metrics::default();

                for item in notifications.into_iter().take(MAX_TELEGRAM_SENDS_PER_RUN) {
                    if is_near_cpu_limit() {
                        console_warn!("Near CPU limit during Telegram dispatch; deferring remaining.");
                        break;
                    }

                    if let Ok(m) =
                        db::mark_notification_attempt(db_ref, session_ref, &item.notice.url).await
                    {
                        metrics.rows_read += m.rows_read;
                        metrics.rows_written += m.rows_written;
                    }

                    match telegram::send_notice_alert(secrets_ref, &item.notice, item.is_edited)
                        .await
                    {
                        Ok(true) => {
                            let m = db::mark_notified(
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
                                "Telegram deferred notice {} due to rate limit; will retry next run.",
                                item.notice.url
                            );
                            break;
                        }
                        Err(err) => {
                            console_error!(
                                "Telegram send failed for {}: {}",
                                item.notice.url,
                                err
                            );
                        }
                    }
                }

                span.set_attribute("sent", sent_count as i64);
                span.set_status(SpanStatus::Ok);
                Ok::<D1Metrics, worker::Error>(metrics)
            })
            .await?;

        total_d1.rows_read += notify_metrics.rows_read;
        total_d1.rows_written += notify_metrics.rows_written;
    }

    console_log!(
        "[Run Summary] rows_read={}, rows_written={}, silent_backfills={}",
        total_d1.rows_read,
        total_d1.rows_written,
        silent_backfills
    );

    Ok(())
}