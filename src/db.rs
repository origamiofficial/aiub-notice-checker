use chrono::Utc;
use std::collections::HashMap;
use worker::{
    d1::{D1Database, D1DatabaseSession, D1PreparedArgument, D1PreparedStatement, D1Type},
    query, Result,
};

use crate::config::D1_BIND_CHUNK_SIZE;
use crate::models::{D1Metrics, Notice, NoticeRow};

/// Loads existing D1 rows strictly for the URLs currently present in the 100-notice listing.
/// Uses `D1PreparedStatement::bind_refs` (`worker 0.9.0`) with borrowed `D1Type` slices.
pub async fn get_notices_by_links(
    session: &D1DatabaseSession,
    links: &[String],
) -> Result<(HashMap<String, NoticeRow>, D1Metrics)> {
    let mut map = HashMap::with_capacity(links.len());
    let mut metrics = D1Metrics::default();

    if links.is_empty() {
        return Ok((map, metrics));
    }

    for chunk in links.chunks(D1_BIND_CHUNK_SIZE) {
        let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT title, description, link, published_date, body_text, attachments_json, \
             content_hash, sent_at, last_notified_hash, detail_checked_at, detail_fetched_at \
             FROM notices WHERE link IN ({})",
            placeholders.join(", ")
        );

        let d1_types: Vec<D1Type> = chunk.iter().map(|l| D1Type::Text(l.as_str())).collect();
        let args: Vec<D1PreparedArgument> = d1_types.iter().map(D1PreparedArgument::new).collect();

        let stmt = session.prepare(&sql).bind_refs(&args)?;
        let result = stmt.all().await?;

        if let Ok(Some(meta)) = result.meta() {
            metrics.rows_read += meta.rows_read.unwrap_or(0);
            metrics.rows_written += meta.rows_written.unwrap_or(0);
        }

        let rows: Vec<NoticeRow> = result.results()?;
        for row in rows {
            map.insert(row.link.clone(), row);
        }
    }

    Ok((map, metrics))
}

/// Builds a parameterized upsert statement using `worker::query!` (`&D1Database`).
pub fn build_upsert_statement(
    db: &D1Database,
    notice: &Notice,
    content_hash: &str,
    detail_attempted: bool,
) -> Result<D1PreparedStatement> {
    let now = Utc::now().to_rfc3339();
    let pub_date = notice.published_date.as_deref().unwrap_or("");
    let attachments_json = notice.attachments_json();

    if detail_attempted && notice.detail_fetched {
        query!(
            db,
            "INSERT INTO notices (
                title, description, link, published_date, body_text, attachments_json,
                content_hash, first_seen_at, last_seen_at, detail_checked_at, detail_fetched_at,
                content_changed_at
            ) VALUES (?1, ?2, ?3, NULLIF(?4, ''), ?5, ?6, ?7, ?8, ?8, ?8, ?8, ?8)
            ON CONFLICT(link) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                published_date = COALESCE(excluded.published_date, notices.published_date),
                body_text = excluded.body_text,
                attachments_json = excluded.attachments_json,
                content_changed_at = CASE
                    WHEN notices.content_hash != excluded.content_hash THEN excluded.content_changed_at
                    ELSE notices.content_changed_at
                END,
                content_hash = excluded.content_hash,
                last_seen_at = excluded.last_seen_at,
                detail_checked_at = excluded.detail_checked_at,
                detail_fetched_at = excluded.detail_fetched_at",
            &notice.title,
            &notice.description,
            &notice.url,
            pub_date,
            &notice.body_text,
            &attachments_json,
            content_hash,
            &now,
        )
    } else if detail_attempted {
        query!(
            db,
            "INSERT INTO notices (
                title, description, link, published_date, body_text, attachments_json,
                content_hash, first_seen_at, last_seen_at, detail_checked_at, detail_fetched_at,
                content_changed_at
            ) VALUES (?1, ?2, ?3, NULLIF(?4, ''), '', '[]', ?5, ?6, ?6, ?6, NULL, ?6)
            ON CONFLICT(link) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                published_date = COALESCE(excluded.published_date, notices.published_date),
                content_changed_at = CASE
                    WHEN notices.content_hash != excluded.content_hash THEN excluded.content_changed_at
                    ELSE notices.content_changed_at
                END,
                content_hash = excluded.content_hash,
                last_seen_at = excluded.last_seen_at,
                detail_checked_at = excluded.detail_checked_at",
            &notice.title,
            &notice.description,
            &notice.url,
            pub_date,
            content_hash,
            &now,
        )
    } else {
        query!(
            db,
            "INSERT INTO notices (
                title, description, link, published_date, body_text, attachments_json,
                content_hash, first_seen_at, last_seen_at, detail_checked_at, detail_fetched_at,
                content_changed_at
            ) VALUES (?1, ?2, ?3, NULLIF(?4, ''), '', '[]', ?5, ?6, ?6, NULL, NULL, ?6)
            ON CONFLICT(link) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                published_date = COALESCE(excluded.published_date, notices.published_date),
                content_changed_at = CASE
                    WHEN notices.content_hash != excluded.content_hash THEN excluded.content_changed_at
                    ELSE notices.content_changed_at
                END,
                content_hash = excluded.content_hash,
                last_seen_at = excluded.last_seen_at",
            &notice.title,
            &notice.description,
            &notice.url,
            pub_date,
            content_hash,
            &now,
        )
    }
}

/// Builds a lightweight UPDATE that only touches timestamp columns, leaving indexed
/// fields (`published_date`, `content_hash`, etc.) untouched.  Used when a rotation
/// detail-check confirms the content hasn't changed, cutting D1 `rows_written` from
/// ~3 (b-tree rewrites on indexed columns) to 1.
pub fn build_lightweight_detail_update(
    db: &D1Database,
    link: &str,
    detail_fetched: bool,
) -> Result<D1PreparedStatement> {
    let now = Utc::now().to_rfc3339();
    if detail_fetched {
        query!(
            db,
            "UPDATE notices SET last_seen_at = ?1, detail_checked_at = ?1, \
             detail_fetched_at = COALESCE(?1, detail_fetched_at) WHERE link = ?2",
            &now,
            link,
        )
    } else {
        query!(
            db,
            "UPDATE notices SET last_seen_at = ?1, detail_checked_at = ?1 WHERE link = ?2",
            &now,
            link,
        )
    }
}

/// Executes a batch of prepared statements on the `D1DatabaseSession` and aggregates `D1Metrics`.
pub async fn execute_batch(
    session: &D1DatabaseSession,
    statements: Vec<D1PreparedStatement>,
) -> Result<D1Metrics> {
    let mut metrics = D1Metrics::default();
    if statements.is_empty() {
        return Ok(metrics);
    }

    for chunk in statements.chunks(D1_BIND_CHUNK_SIZE) {
        let results = session.batch(chunk.to_vec()).await?;
        for res in results {
            if let Ok(Some(meta)) = res.meta() {
                metrics.rows_read += meta.rows_read.unwrap_or(0);
                metrics.rows_written += meta.rows_written.unwrap_or(0);
            }
        }
    }

    Ok(metrics)
}

/// Records the timestamp of a Telegram send attempt.
pub async fn mark_notification_attempt(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET notification_attempted_at=?1 WHERE link=?2",
        &now,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Marks a notice as notified for the given `content_hash`.
pub async fn mark_notified(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET sent_at=COALESCE(sent_at, ?1), last_notified_hash=?2 WHERE link=?3",
        &now,
        content_hash,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}
