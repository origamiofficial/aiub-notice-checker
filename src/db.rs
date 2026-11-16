use chrono::{Duration, Utc};
use worker::d1::{
    D1Database, D1DatabaseSession, D1PreparedStatement, D1Result, D1SessionConstraint, D1Type,
};
use worker::Result;

use crate::config::{D1_BIND_CHUNK_SIZE, DETAIL_RETRY_COOLDOWN_MINUTES};
use crate::models::{D1Metrics, Notice, NoticeRow};

pub fn utc_now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

pub fn retry_cutoff_iso() -> String {
    (Utc::now() - Duration::minutes(DETAIL_RETRY_COOLDOWN_MINUTES))
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

fn checked_type(value: Option<&str>) -> D1Type<'_> {
    value.map_or(D1Type::Null, D1Type::Text)
}

pub fn start_primary_session(db: &D1Database) -> Result<D1DatabaseSession> {
    db.with_session_constraint(D1SessionConstraint::FirstPrimary)
}

fn accumulate_meta(results: &[D1Result]) -> D1Metrics {
    let mut metrics = D1Metrics::default();
    for res in results {
        if let Ok(Some(meta)) = res.meta() {
            metrics.rows_read += meta.rows_read.unwrap_or(0);
            metrics.rows_written += meta.rows_written.unwrap_or(0);
        }
    }
    metrics
}

/// Fetches existing D1 rows strictly for the 100 active listing links
/// using `D1Type` + `bind_refs` and a single `session.batch()` call.
pub async fn get_notices_by_links(
    session: &D1DatabaseSession,
    links: &[String],
) -> Result<(Vec<NoticeRow>, D1Metrics)> {
    if links.is_empty() {
        return Ok((Vec::new(), D1Metrics::default()));
    }

    let mut statements = Vec::new();
    for chunk in links.chunks(D1_BIND_CHUNK_SIZE) {
        let placeholders = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT title, description, link, published_date, body_text, attachments_json, \
             content_hash, sent_at, last_notified_hash, detail_checked_at, detail_fetched_at \
             FROM notices WHERE link IN ({placeholders})"
        );
        let args: Vec<D1Type> = chunk.iter().map(|l| D1Type::Text(l.as_str())).collect();
        statements.push(session.prepare(&sql).bind_refs(&args)?);
    }

    let batch_results = session.batch(statements).await?;
    let metrics = accumulate_meta(&batch_results);
    let mut rows = Vec::with_capacity(links.len());
    for res in batch_results {
        rows.extend(res.results::<NoticeRow>()?);
    }
    Ok((rows, metrics))
}

/// Uses `worker::query!` to build typed INSERT/UPDATE statements only for modified notices.
pub fn build_upsert_statement(
    session: &D1DatabaseSession,
    notice: &Notice,
    new_hash: &str,
    existing: Option<&NoticeRow>,
    detail_checked: bool,
    silent_backfill: bool,
    now: &str,
) -> Result<Option<D1PreparedStatement>> {
    let attachments_json = notice.attachments_json();
    let checked_ts = detail_checked.then_some(now);
    let fetched_ts = notice.detail_fetched.then_some(now);

    match existing {
        None => {
            let stmt = session
                .prepare(
                    "INSERT INTO notices \
                 (title, description, link, published_date, body_text, attachments_json, \
                  content_hash, first_seen_at, last_seen_at, content_changed_at, \
                  detail_checked_at, detail_fetched_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .bind_refs(&[
                    D1Type::Text(&notice.title),
                    D1Type::Text(&notice.description),
                    D1Type::Text(&notice.url),
                    checked_type(notice.published_date.as_deref()),
                    D1Type::Text(&notice.body_text),
                    D1Type::Text(&attachments_json),
                    D1Type::Text(new_hash),
                    D1Type::Text(now),
                    D1Type::Text(now),
                    D1Type::Text(now),
                    checked_type(checked_ts),
                    checked_type(fetched_ts),
                ])?;
            Ok(Some(stmt))
        }
        Some(prev) if prev.content_hash != new_hash => {
            let backfill_flag: i32 = if silent_backfill { 1 } else { 0 };
            let stmt = session
                .prepare(
                "UPDATE notices SET title=?1, description=?2, published_date=?3, \
                 body_text=?4, attachments_json=?5, content_hash=?6, last_seen_at=?7, \
                 content_changed_at=CASE WHEN ?8 = 1 THEN content_changed_at ELSE ?7 END, \
                 last_notified_hash=CASE WHEN ?8 = 1 THEN ?6 ELSE last_notified_hash END, \
                 notification_attempted_at=CASE WHEN ?8 = 1 THEN notification_attempted_at ELSE NULL END, \
                 detail_checked_at=COALESCE(?9, detail_checked_at), \
                 detail_fetched_at=COALESCE(?10, detail_fetched_at) WHERE link=?11",
                )
                .bind_refs(&[
                    D1Type::Text(&notice.title),
                    D1Type::Text(&notice.description),
                    checked_type(notice.published_date.as_deref()),
                    D1Type::Text(&notice.body_text),
                    D1Type::Text(&attachments_json),
                    D1Type::Text(new_hash),
                    D1Type::Text(now),
                    D1Type::Integer(backfill_flag),
                    checked_type(checked_ts),
                    checked_type(fetched_ts),
                    D1Type::Text(&notice.url),
                ])?;
            Ok(Some(stmt))
        }
        Some(_) if detail_checked => {
            let stmt = session
                .prepare(
                    "UPDATE notices SET detail_checked_at=?1, \
                 detail_fetched_at=COALESCE(?2, detail_fetched_at) WHERE link=?3",
                )
                .bind_refs(&[
                    D1Type::Text(now),
                    checked_type(fetched_ts),
                    D1Type::Text(&notice.url),
                ])?;
            Ok(Some(stmt))
        }
        Some(_) => Ok(None),
    }
}

pub async fn execute_batch(
    session: &D1DatabaseSession,
    statements: Vec<D1PreparedStatement>,
) -> Result<D1Metrics> {
    if statements.is_empty() {
        return Ok(D1Metrics::default());
    }
    let mut totals = D1Metrics::default();
    for chunk in statements.chunks(D1_BIND_CHUNK_SIZE) {
        let res = session.batch(chunk.to_vec()).await?;
        let chunk_metrics = accumulate_meta(&res);
        totals.rows_read += chunk_metrics.rows_read;
        totals.rows_written += chunk_metrics.rows_written;
    }
    Ok(totals)
}

pub async fn mark_notification_attempt(session: &D1DatabaseSession, link: &str) -> Result<()> {
    let now = utc_now_iso();
    session
        .prepare("UPDATE notices SET notification_attempted_at=?1 WHERE link=?2")
        .bind_refs(&[D1Type::Text(&now), D1Type::Text(link)])?
        .run()
        .await?;
    Ok(())
}

pub async fn mark_notified(
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<()> {
    let now = utc_now_iso();
    session
        .prepare(
            "UPDATE notices SET sent_at=COALESCE(sent_at, ?1), last_notified_hash=?2 WHERE link=?3",
        )
        .bind_refs(&[
            D1Type::Text(&now),
            D1Type::Text(content_hash),
            D1Type::Text(link),
        ])?
        .run()
        .await?;
    Ok(())
}
