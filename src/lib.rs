mod config;
mod db;
mod models;
mod scraper;
mod telegram;

use std::collections::{HashMap, HashSet};
use worker::{console_error, console_log, event, Env, Result, ScheduleContext, ScheduledEvent};

use crate::config::{
    Secrets, DETAIL_FAILURE_MIN_SAMPLE, DETAIL_FAILURE_RATIO, MAX_DETAIL_FETCHES_PER_RUN,
    MAX_TELEGRAM_SENDS_PER_RUN,
};
use crate::models::{format_notice_message, titles_equivalent, Notice, NoticeRow};

#[event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(err) = run(&env).await {
        console_error!("Scheduled notice check failed: {err}");
    }
}

async fn run(env: &Env) -> Result<()> {
    let secrets = Secrets::from_env(env)?;
    let database = env.d1("DB")?;

    let listing_url = scraper::listing_page_url(1);
    let listing_html = match scraper::fetch_html(&listing_url).await {
        Ok(html) => html,
        Err(err) => {
            telegram::send_admin_alert(
                &secrets.telegram_bot_api_key,
                secrets.telegram_admin_chat_id.as_deref(),
                "AIUB Notice\n\nFailed to fetch the AIUB notices listing page. Will retry on the next 5-minute run.",
            )
            .await;
            return Err(err);
        }
    };

    let listing_notices = match scraper::parse_listing_page(&listing_html) {
        Ok(list) => list,
        Err(err) => {
            telegram::send_admin_alert(
                &secrets.telegram_bot_api_key,
                secrets.telegram_admin_chat_id.as_deref(),
                "AIUB Notice\n\nAIUB listing parser validation failed. The site HTML structure may have changed.",
            )
            .await;
            return Err(err);
        }
    };

    let links: Vec<String> = listing_notices.iter().map(|n| n.url.clone()).collect();
    let mut existing_map: HashMap<String, NoticeRow> = db::get_notices_by_links(&database, &links)
        .await?
        .into_iter()
        .map(|row| (row.link.clone(), row))
        .collect();

    // Select up to MAX_DETAIL_FETCHES_PER_RUN targets: new notices first, then pending detail rows.
    let mut detail_targets: Vec<Notice> = Vec::new();
    let mut selected_links: HashSet<String> = HashSet::new();

    for notice in &listing_notices {
        if !existing_map.contains_key(&notice.url) && detail_targets.len() < MAX_DETAIL_FETCHES_PER_RUN {
            selected_links.insert(notice.url.clone());
            detail_targets.push(notice.clone());
        }
    }

    if detail_targets.len() < MAX_DETAIL_FETCHES_PER_RUN {
        let remaining = MAX_DETAIL_FETCHES_PER_RUN - detail_targets.len();
        let pending_rows = db::get_pending_detail_rows(&database, remaining).await?;
        for row in pending_rows {
            if !selected_links.contains(&row.link) {
                selected_links.insert(row.link.clone());
                let fallback = listing_notices
                    .iter()
                    .find(|n| n.url == row.link)
                    .cloned()
                    .unwrap_or(Notice {
                        title: row.title.clone(),
                        description: row.description.clone(),
                        url: row.link.clone(),
                        published_date: row.published_date.clone(),
                        body_text: row.body_text.clone(),
                        attachments: row.parsed_attachments(),
                        detail_fetched: false,
                    });
                existing_map.entry(row.link.clone()).or_insert(row);
                detail_targets.push(fallback);
            }
        }
    }

    let mut enriched_map: HashMap<String, Notice> = HashMap::new();
    let mut detail_failures = 0usize;

    for target in &detail_targets {
        match scraper::fetch_html(&target.url).await {
            Ok(html) => match scraper::parse_detail_page(&html, target) {
                Ok(enriched) => {
                    enriched_map.insert(target.url.clone(), enriched);
                }
                Err(err) => {
                    detail_failures += 1;
                    console_error!("Detail parse failed for {}: {err}", target.url);
                }
            },
            Err(err) => {
                detail_failures += 1;
                console_error!("Detail fetch failed for {}: {err}", target.url);
            }
        }
    }

    if detail_targets.len() >= DETAIL_FAILURE_MIN_SAMPLE
        && (detail_failures as f64) / (detail_targets.len() as f64) >= DETAIL_FAILURE_RATIO
    {
        telegram::send_admin_alert(
            &secrets.telegram_bot_api_key,
            secrets.telegram_admin_chat_id.as_deref(),
            "AIUB Notice\n\nMost sampled detail pages failed validation. The detail parser may need an update.",
        )
        .await;
        return Err(worker::Error::RustError("Detail-page health check failed".into()));
    }

    // Merge listing notices with any off-page retries so failed sends are retried.
    let retry_rows = db::get_unsent_retry_rows(&database, MAX_TELEGRAM_SENDS_PER_RUN).await?;
    let mut worklist: Vec<Notice> = Vec::new();
    let mut seen_work: HashSet<String> = HashSet::new();

    for row in retry_rows {
        if seen_work.insert(row.link.clone()) {
            let fallback = listing_notices
                .iter()
                .find(|n| n.url == row.link)
                .cloned()
                .unwrap_or(Notice {
                    title: row.title.clone(),
                    description: row.description.clone(),
                    url: row.link.clone(),
                    published_date: row.published_date.clone(),
                    body_text: row.body_text.clone(),
                    attachments: row.parsed_attachments(),
                    detail_fetched: false,
                });
            existing_map.entry(row.link.clone()).or_insert(row);
            worklist.push(fallback);
        }
    }
    for notice in listing_notices {
        if seen_work.insert(notice.url.clone()) {
            worklist.push(notice);
        }
    }

    let mut sends = 0usize;
    let mut new_count = 0usize;
    let mut edited_count = 0usize;

    for listing in worklist {
        let existing = existing_map.get(&listing.url);
        let checked = selected_links.contains(&listing.url);
        let effective = match enriched_map.get(&listing.url) {
            Some(enriched) => enriched.clone(),
            None => merge_cached(&listing, existing),
        };
        let new_hash = effective.content_hash();

        db::upsert_notice(&database, &effective, &new_hash, existing, checked).await?;

        let (should_send, edited) = match existing {
            None => {
                new_count += 1;
                (true, false)
            }
            Some(prev) if prev.sent_at.is_none() => (true, false),
            Some(prev) if prev.last_notified_hash.as_deref() != Some(new_hash.as_str()) => {
                edited_count += 1;
                (true, true)
            }
            Some(_) => (false, false),
        };

        if !should_send || sends >= MAX_TELEGRAM_SENDS_PER_RUN {
            continue;
        }

        sends += 1;
        db::mark_notification_attempt(&database, &effective.url).await?;
        let msg = format_notice_message(&effective, edited);
        if telegram::send_message(&secrets.telegram_bot_api_key, &secrets.telegram_chat_id, &msg).await? {
            db::mark_notified(&database, &effective.url, &new_hash).await?;
        }
    }

    console_log!("Run complete: new={new_count}, edited={edited_count}, sent={sends}");
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
        published_date: listing.published_date.clone().or_else(|| prev.published_date.clone()),
        body_text: prev.body_text.clone(),
        attachments: prev.parsed_attachments(),
        detail_fetched: false,
    }
}