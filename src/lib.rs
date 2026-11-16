mod config;
mod db;
mod models;
mod scraper;
mod telegram;

use std::collections::{HashMap, HashSet};
use worker::observability::{enter_span_async, SpanStatus};
use worker::{
    console_error, console_log, event, signal, Env, Result, ScheduleContext, ScheduledEvent,
};

use crate::config::{
    Secrets, DETAIL_FAILURE_MIN_SAMPLE, DETAIL_FAILURE_RATIO, MAX_DETAIL_FETCHES_PER_RUN,
    MAX_TELEGRAM_SENDS_PER_RUN,
};
use crate::models::{format_notice_message, titles_equivalent, Notice, NoticeRow};
use crate::telegram::SendOutcome;

#[event(scheduled)]
pub async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let cron_expr = event.cron();
    let schedule_ts = event.schedule();

    if let Err(err) = enter_span_async("scheduled_notice_check", |span| async move {
        span.set_attribute("cron", cron_expr.as_str());
        span.set_attribute("scheduled_time_ms", schedule_ts.to_string().as_str());
        let res = run(&env).await;
        if let Err(ref e) = res {
            span.set_status(SpanStatus::Error(Some(e.to_string())));
        }
        res
    })
    .await
    {
        console_error!("Scheduled notice check failed: {err}");
    }
}

async fn run(env: &Env) -> Result<()> {
    let secrets = Secrets::from_env(env)?;
    let database = env.d1("DB")?;
    let session = db::start_primary_session(&database)?;

    // 1. Fetch & parse the 100 notices on listing page 1.
    let bot_api_key = &secrets.telegram_bot_api_key;
    let admin_chat_id = secrets.telegram_admin_chat_id.as_deref();
    let listing_notices = enter_span_async("fetch_listing", |span| async move {
        let listing_url = scraper::listing_page_url(1);
        span.set_attribute("url", listing_url.as_str());
        let html = match scraper::fetch_html(&listing_url).await {
            Ok(h) => h,
            Err(err) => {
                span.set_status(SpanStatus::Error(Some(err.to_string())));
                telegram::send_admin_alert(
                    bot_api_key,
                    admin_chat_id,
                    "AIUB Notice\n\nFailed to fetch the AIUB notices listing page. Will retry on the next 5-minute run.",
                )
                .await;
                return Err(err);
            }
        };

        match scraper::parse_listing_page(&html) {
            Ok(list) => Ok(list),
            Err(err) => {
                span.set_status(SpanStatus::Error(Some(err.to_string())));
                telegram::send_admin_alert(
                    bot_api_key,
                    admin_chat_id,
                    "AIUB Notice\n\nAIUB listing parser validation failed. The site HTML structure may have changed.",
                )
                .await;
                Err(err)
            }
        }
    })
    .await?;

    // 2. Load D1 state strictly for these 100 listing URLs via D1DatabaseSession.
    let links: Vec<String> = listing_notices.iter().map(|n| n.url.clone()).collect();
    let session_ref = &session;
    let (existing_rows, read_metrics) = enter_span_async("d1_load_100", |span| async move {
        let (rows, metrics) = db::get_notices_by_links(session_ref, &links).await?;
        span.set_attribute("rows_read", metrics.rows_read.to_string().as_str());
        Ok::<_, worker::Error>((rows, metrics))
    })
    .await?;

    let existing_map: HashMap<String, NoticeRow> = existing_rows
        .into_iter()
        .map(|row| (row.link.clone(), row))
        .collect();

    // 3. Pick up to MAX_DETAIL_FETCHES_PER_RUN targets strictly from the 100 listing notices:
    //    (a) Brand-new notices not in D1
    //    (b) Notices in the 100 with detail_fetched_at IS NULL (respecting retry cooldown)
    //    (c) Rotation across the 100 notices ordered by oldest detail_checked_at
    let retry_cutoff = db::retry_cutoff_iso();
    let mut detail_targets: Vec<Notice> = Vec::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);
    let mut selected_links: HashSet<String> = HashSet::with_capacity(MAX_DETAIL_FETCHES_PER_RUN);

    for notice in &listing_notices {
        if detail_targets.len() >= MAX_DETAIL_FETCHES_PER_RUN {
            break;
        }
        if !existing_map.contains_key(&notice.url) && selected_links.insert(notice.url.clone()) {
            detail_targets.push(notice.clone());
        }
    }

    if detail_targets.len() < MAX_DETAIL_FETCHES_PER_RUN {
        for notice in &listing_notices {
            if detail_targets.len() >= MAX_DETAIL_FETCHES_PER_RUN {
                break;
            }
            if let Some(row) = existing_map.get(&notice.url) {
                if row.detail_fetched_at.is_none()
                    && row.detail_due(&retry_cutoff)
                    && selected_links.insert(notice.url.clone())
                {
                    detail_targets.push(notice.clone());
                }
            }
        }
    }

    if detail_targets.len() < MAX_DETAIL_FETCHES_PER_RUN {
        let mut rotation_candidates: Vec<&Notice> = listing_notices
            .iter()
            .filter(|n| {
                !selected_links.contains(&n.url)
                    && existing_map
                        .get(&n.url)
                        .map(|r| r.detail_due(&retry_cutoff))
                        .unwrap_or(false)
            })
            .collect();

        rotation_candidates.sort_by(|a, b| {
            let a_ts = existing_map
                .get(&a.url)
                .and_then(|r| r.detail_checked_at.as_deref())
                .unwrap_or("");
            let b_ts = existing_map
                .get(&b.url)
                .and_then(|r| r.detail_checked_at.as_deref())
                .unwrap_or("");
            a_ts.cmp(b_ts)
        });

        for notice in rotation_candidates {
            if detail_targets.len() >= MAX_DETAIL_FETCHES_PER_RUN {
                break;
            }
            if selected_links.insert(notice.url.clone()) {
                detail_targets.push(notice.clone());
            }
        }
    }

    // 4. Fetch & parse selected detail pages, stopping early if approaching the 10ms CPU limit.
    let (enriched_map, checked_links, detail_failures) =
        enter_span_async("fetch_details", |span| async move {
            let mut enriched = HashMap::new();
            let mut checked = HashSet::new();
            let mut failures = 0usize;

            for target in &detail_targets {
                if signal::is_near_cpu_limit() {
                    console_log!("Near CPU limit; deferring remaining detail fetches to next run.");
                    break;
                }
                checked.insert(target.url.clone());
                match scraper::fetch_html(&target.url).await {
                    Ok(html) => match scraper::parse_detail_page(&html, target) {
                        Ok(parsed) => {
                            enriched.insert(target.url.clone(), parsed);
                        }
                        Err(err) => {
                            failures += 1;
                            console_error!("Detail parse failed for {}: {err}", target.url);
                        }
                    },
                    Err(err) => {
                        failures += 1;
                        console_error!("Detail fetch failed for {}: {err}", target.url);
                    }
                }
            }
            span.set_attribute("checked_count", checked.len().to_string().as_str());
            span.set_attribute("failure_count", failures.to_string().as_str());
            (enriched, checked, failures)
        })
        .await;

    if checked_links.len() >= DETAIL_FAILURE_MIN_SAMPLE
        && (detail_failures as f64) / (checked_links.len() as f64) >= DETAIL_FAILURE_RATIO
    {
        telegram::send_admin_alert(
            &secrets.telegram_bot_api_key,
            secrets.telegram_admin_chat_id.as_deref(),
            "AIUB Notice\n\nMost sampled detail pages failed validation. The detail parser may need an update.",
        )
        .await;
        return Err(worker::Error::RustError(
            "Detail-page health check failed".into(),
        ));
    }

    // 5. Build batched D1 upserts and queue notifications strictly across the 100 notices.
    let now = db::utc_now_iso();
    let mut upsert_statements = Vec::new();
    let mut pending_deliveries: Vec<(Notice, String, bool)> = Vec::new();
    let mut new_count = 0usize;
    let mut edited_count = 0usize;

    for listing in &listing_notices {
        let existing = existing_map.get(&listing.url);
        let checked = checked_links.contains(&listing.url);
        let enriched = enriched_map.get(&listing.url);
        let effective = match enriched {
            Some(e) => e.clone(),
            None => merge_cached(listing, existing),
        };
        let new_hash = effective.content_hash();

        let listing_changed = existing.map_or(false, |prev| {
            let title_changed = enriched.map_or(false, |e| {
                e.title != prev.title
                    && (prev.detail_fetched_at.is_some()
                        || !titles_equivalent(&listing.title, &prev.title))
            });
            title_changed
                || listing.description != prev.description
                || (listing.published_date.is_some()
                    && listing.published_date != prev.published_date)
        });

        let silent_backfill = existing.map_or(false, |prev| {
            prev.sent_at.is_some()
                && prev.detail_fetched_at.is_none()
                && enriched.is_some()
                && !listing_changed
                && prev.last_notified_hash.as_deref() == Some(prev.content_hash.as_str())
        });

        if let Some(stmt) = db::build_upsert_statement(
            &session,
            &effective,
            &new_hash,
            existing,
            checked,
            silent_backfill,
            &now,
        )? {
            upsert_statements.push(stmt);
        }

        if silent_backfill {
            continue;
        }

        let detail_ready_or_fallback = effective.detail_fetched
            || checked
            || existing
                .and_then(|r| r.detail_checked_at.as_ref())
                .is_some();

        match existing {
            None => {
                new_count += 1;
                if detail_ready_or_fallback {
                    pending_deliveries.push((effective, new_hash, false));
                }
            }
            Some(prev) if prev.sent_at.is_none() => {
                if detail_ready_or_fallback {
                    pending_deliveries.push((effective, new_hash, false));
                }
            }
            Some(prev) if prev.last_notified_hash.as_deref() != Some(new_hash.as_str()) => {
                edited_count += 1;
                pending_deliveries.push((effective, new_hash, true));
            }
            Some(_) => {}
        }
    }

    let upserted_count = upsert_statements.len();
    let session_ref = &session;
    let write_metrics = enter_span_async("d1_sync", |span| async move {
        span.set_attribute("statements", upserted_count.to_string().as_str());
        let metrics = db::execute_batch(session_ref, upsert_statements).await?;
        span.set_attribute("rows_read", metrics.rows_read.to_string().as_str());
        span.set_attribute("rows_written", metrics.rows_written.to_string().as_str());
        Ok::<_, worker::Error>(metrics)
    })
    .await?;

    // 6. Deliver up to MAX_TELEGRAM_SENDS_PER_RUN notifications with Delay pacing and 429 handling.
    let mut sends = 0usize;
    let session_ref = &session;
    let bot_api_key = &secrets.telegram_bot_api_key;
    enter_span_async("telegram_delivery", |span| async move {
        for (idx, (notice, new_hash, edited)) in pending_deliveries
            .into_iter()
            .take(MAX_TELEGRAM_SENDS_PER_RUN)
            .enumerate()
        {
            if idx > 0 {
                telegram::pace_between_messages().await;
            }
            sends += 1;
            db::mark_notification_attempt(session_ref, &notice.url).await?;
            let msg = format_notice_message(&notice, edited);
            match telegram::send_message(bot_api_key, &secrets.telegram_chat_id, &msg).await? {
                SendOutcome::Delivered => {
                    db::mark_notified(session_ref, &notice.url, &new_hash).await?;
                }
                SendOutcome::Failed => {}
                SendOutcome::RateLimited => break,
            }
        }
        span.set_attribute("sent_count", sends.to_string().as_str());
        Ok::<(), worker::Error>(())
    })
    .await?;

    console_log!(
        "Run complete (100-notice scope): detail_checked={}, d1_rows_read={}, d1_rows_written={}, new={}, edited={}, sent={}",
        checked_links.len(),
        read_metrics.rows_read + write_metrics.rows_read,
        write_metrics.rows_written,
        new_count,
        edited_count,
        sends
    );
    Ok(())
}

fn merge_cached(listing: &Notice, existing: Option<&NoticeRow>) -> Notice {
    let Some(prev) = existing else {
        return listing.clone();
    };
    let title = if !prev.title.is_empty() && titles_equivalent(&listing.title, &prev.title) {
        prev.title.clone()
    } else {
        listing.title.clone()
    };
    Notice {
        title,
        description: listing.description.clone(),
        url: listing.url.clone(),
        published_date: listing
            .published_date
            .clone()
            .or_else(|| prev.published_date.clone()),
        body_text: prev.body_text.clone(),
        attachments: prev.parsed_attachments(),
        detail_fetched: false,
    }
}
