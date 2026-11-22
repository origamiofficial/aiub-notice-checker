use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use worker::{
    d1::{D1Database, D1DatabaseSession, D1PreparedArgument, D1PreparedStatement, D1Type},
    query, Result,
};

use crate::config::D1_BIND_CHUNK_SIZE;
use crate::models::{D1Metrics, Notice, NoticeRow};

/// Loads existing D1 rows strictly for the URLs currently present in the listing.
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
             content_hash, tg_sent_at, tg_last_notified_hash, detail_checked_at, detail_fetched_at, \
             fb_sent_at, fb_last_notified_hash \
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

/// The SQL used by [`build_lightweight_detail_update`], extracted as a pure
/// function so its semantics can be tested behaviourally against a real
/// SQLite database in tests (L2) without a Cloudflare D1 binding.
///
/// The `COALESCE(detail_fetched_at, ?1)` form is deliberate: it preserves
/// an existing `detail_fetched_at` and only sets it when it is NULL. The
/// inverted form `COALESCE(?1, detail_fetched_at)` would overwrite the
/// timestamp on every lightweight update (fixed in v6.2.0).
///
/// Test-only: the production builder below duplicates this SQL literally
/// because the `worker::query!` macro requires a string literal — keep the
/// two in sync if either changes.
#[cfg(test)]
pub fn lightweight_detail_update_sql(detail_fetched: bool) -> &'static str {
    if detail_fetched {
        "UPDATE notices SET last_seen_at = ?1, detail_checked_at = ?1, \
         detail_fetched_at = COALESCE(detail_fetched_at, ?1) WHERE link = ?2"
    } else {
        "UPDATE notices SET last_seen_at = ?1, detail_checked_at = ?1 WHERE link = ?2"
    }
}

/// Test-only SQL mirrors for the silent-sync / duplicate-handled
/// statements, so their semantics can be tested behaviourally against a
/// real SQLite database in tests (same pattern as
/// `lightweight_detail_update_sql()` above).
///
/// The production builders duplicate these SQL literals because the
/// `worker::query!` macro requires a string literal — keep the two in
/// sync if either changes.
///
/// - `tg_hash_sync_sql()`: H1 — updates only the hash, only for rows
///   Telegram has already sent; it must never set `tg_sent_at`.
/// - `fb_hash_sync_sql()`: the Facebook mirror (guarded on `fb_sent_at`).
/// - `fb_duplicate_handled_sql()`: H2 — a 506 means Meta already has
///   this content, so it also sets `fb_sent_at` (COALESCE, preserving
///   an existing timestamp) so the notice is not re-queued as new.
#[cfg(test)]
pub fn tg_hash_sync_sql() -> &'static str {
    "UPDATE notices SET tg_last_notified_hash=?1 WHERE link=?2 AND tg_sent_at IS NOT NULL"
}

#[cfg(test)]
pub fn fb_hash_sync_sql() -> &'static str {
    "UPDATE notices SET fb_last_notified_hash=?1 WHERE link=?2 AND fb_sent_at IS NOT NULL"
}

#[cfg(test)]
pub fn fb_duplicate_handled_sql() -> &'static str {
    "UPDATE notices SET fb_sent_at=COALESCE(fb_sent_at, ?1), fb_last_notified_hash=?2, fb_notification_attempted_at=?3 WHERE link=?4"
}

/// Builds a lightweight UPDATE that only touches timestamp columns, leaving indexed
/// fields (`published_date`, `content_hash`, etc.) untouched.
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
             detail_fetched_at = COALESCE(detail_fetched_at, ?1) WHERE link = ?2",
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
///
/// D1 `batch()` atomicity (M4): a single `batch()` call is atomic — all of
/// its statements apply, or none do (Cloudflare D1 docs: "All queries
/// execute in single transaction"). Atomicity does NOT extend across
/// chunks: each 50-statement chunk is its own transaction, so a failure in
/// a later chunk never rolls back an earlier chunk. A partial chunk can
/// therefore never leave a notice half-synced *within* that chunk.
///
/// Failure granularity (M4): if a whole chunk fails (e.g. one bad statement
/// poisons the batch), fall back to executing that chunk's statements one
/// at a time, log each per-statement failure, and continue with the
/// remaining chunks instead of aborting the whole run / dispatch. Only if
/// every statement in a chunk also fails individually is the error
/// propagated, so a single bad row cannot silently discard the rest.
pub async fn execute_batch(
    session: &D1DatabaseSession,
    statements: Vec<D1PreparedStatement>,
) -> Result<D1Metrics> {
    let mut metrics = D1Metrics::default();
    if statements.is_empty() {
        return Ok(metrics);
    }

    for (chunk_idx, chunk) in statements.chunks(D1_BIND_CHUNK_SIZE).enumerate() {
        match session.batch(chunk.to_vec()).await {
            Ok(results) => {
                for res in results {
                    if let Ok(Some(meta)) = res.meta() {
                        metrics.rows_read += meta.rows_read.unwrap_or(0);
                        metrics.rows_written += meta.rows_written.unwrap_or(0);
                    }
                }
            }
            Err(batch_err) => {
                worker::console_warn!(
                    "[D1 Batch Fallback] Chunk {} ({} statements) failed as a batch: {}; retrying statements individually.",
                    chunk_idx,
                    chunk.len(),
                    batch_err
                );
                let mut succeeded = 0usize;
                let mut failed = 0usize;
                for (stmt_idx, stmt) in chunk.iter().enumerate() {
                    match session.batch(vec![stmt.clone()]).await {
                        Ok(results) => {
                            succeeded += 1;
                            for res in results {
                                if let Ok(Some(meta)) = res.meta() {
                                    metrics.rows_read += meta.rows_read.unwrap_or(0);
                                    metrics.rows_written += meta.rows_written.unwrap_or(0);
                                }
                            }
                        }
                        Err(stmt_err) => {
                            failed += 1;
                            worker::console_warn!(
                                "[D1 Batch Fallback] Chunk {} statement {} failed individually: {}",
                                chunk_idx,
                                stmt_idx,
                                stmt_err
                            );
                        }
                    }
                }
                if succeeded == 0 {
                    // Every statement in this chunk failed even on its own:
                    // there is nothing salvageable, so propagate the error
                    // instead of silently pretending the run succeeded.
                    worker::console_error!(
                        "[D1 Batch Failed] Chunk {} failed entirely ({} statements, all failed individually); aborting batch.",
                        chunk_idx,
                        chunk.len()
                    );
                    return Err(batch_err);
                }
                worker::console_warn!(
                    "[D1 Batch Fallback] Chunk {} recovered: {}/{} statements applied individually ({} failed).",
                    chunk_idx,
                    succeeded,
                    chunk.len(),
                    failed
                );
            }
        }
    }

    Ok(metrics)
}

// ── Telegram delivery tracking ───────────────────────────────────────

/// Records the timestamp of a Telegram send attempt.
pub async fn mark_tg_notification_attempt(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET tg_notification_attempted_at=?1 WHERE link=?2",
        &now,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Builds an UPDATE statement marking a notice as notified via Telegram.
pub fn build_tg_notified_statement(
    db: &D1Database,
    link: &str,
    content_hash: &str,
    now: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET tg_sent_at=COALESCE(tg_sent_at, ?1), tg_last_notified_hash=?2 WHERE link=?3",
        now,
        content_hash,
        link,
    )
}

/// Builds an UPDATE statement for Telegram silent hash sync.
///
/// H1: unlike `build_tg_notified_statement()` (which sets `tg_sent_at`),
/// this only updates the stored hash, and only for rows Telegram has
/// already sent (`tg_sent_at IS NOT NULL`). A silent backfill must never
/// mark a never-sent notice as sent — that silently lost the first
/// Telegram send for notices Facebook had already posted.
pub fn build_tg_hash_sync_statement(
    db: &D1Database,
    link: &str,
    content_hash: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET tg_last_notified_hash=?1 WHERE link=?2 AND tg_sent_at IS NOT NULL",
        content_hash,
        link,
    )
}

/// Silent hash sync for Telegram (keeps tg_last_notified_hash in sync without re-posting).
#[allow(dead_code)]
pub async fn mark_tg_hash_sync(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<D1Metrics> {
    let stmt = build_tg_hash_sync_statement(db, link, content_hash)?;
    execute_batch(session, vec![stmt]).await
}

/// Marks a notice as notified via Telegram for the given `content_hash`.
pub async fn mark_tg_notified(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = build_tg_notified_statement(db, link, content_hash, &now)?;
    execute_batch(session, vec![stmt]).await
}

// ── Facebook delivery tracking ───────────────────────────────────────

/// Records the timestamp of a Facebook send attempt.
pub async fn mark_fb_notification_attempt(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET fb_notification_attempted_at=?1 WHERE link=?2",
        &now,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Marks a notice as successfully posted to Facebook.
pub async fn mark_fb_notified(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
    post_id: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET fb_sent_at=COALESCE(fb_sent_at, ?1), fb_last_notified_hash=?2, fb_post_id=?3 WHERE link=?4",
        &now,
        content_hash,
        post_id,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Marks a Facebook duplicate (error 506) as handled to stop retry loops.
///
/// H2: also sets `fb_sent_at = COALESCE(fb_sent_at, now)`. Meta has
/// already seen this content on the Page (that is what a 506 means), so
/// the notice counts as sent for queueing purposes. The old version only
/// updated the hash, so `fb_sent_at` stayed NULL and the next run
/// re-queued the notice as new, got 506 again, and looped forever.
pub async fn mark_fb_duplicate_handled(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<D1Metrics> {
    let now = Utc::now().to_rfc3339();
    let stmt = query!(
        db,
        "UPDATE notices SET fb_sent_at=COALESCE(fb_sent_at, ?1), fb_last_notified_hash=?2, fb_notification_attempted_at=?3 WHERE link=?4",
        &now,
        content_hash,
        &now,
        link,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Builds an UPDATE statement for Facebook silent hash sync.
pub fn build_fb_hash_sync_statement(
    db: &D1Database,
    link: &str,
    content_hash: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET fb_last_notified_hash=?1 WHERE link=?2 AND fb_sent_at IS NOT NULL",
        content_hash,
        link,
    )
}

/// Silent hash sync for Facebook (keeps fb_last_notified_hash in sync without re-posting).
#[allow(dead_code)]
pub async fn mark_fb_hash_sync(
    db: &D1Database,
    session: &D1DatabaseSession,
    link: &str,
    content_hash: &str,
) -> Result<D1Metrics> {
    let stmt = build_fb_hash_sync_statement(db, link, content_hash)?;
    execute_batch(session, vec![stmt]).await
}

// ── Operational state (app_state table) ──────────────────────────────

#[derive(Deserialize)]
struct StateRow {
    value: String,
}

/// Reads an operational flag or timestamp from the `app_state` table.
pub async fn get_app_state(session: &D1DatabaseSession, key: &str) -> Result<Option<String>> {
    let stmt = session
        .prepare("SELECT value FROM app_state WHERE key = ?1")
        .bind(&[key.into()])?;
    let row: Option<StateRow> = stmt.first(None).await?;
    Ok(row.map(|r| r.value))
}

/// Persists an operational flag or timestamp in the `app_state` table.
pub async fn set_app_state(
    db: &D1Database,
    session: &D1DatabaseSession,
    key: &str,
    value: &str,
) -> Result<D1Metrics> {
    let stmt = query!(
        db,
        "INSERT INTO app_state (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        key,
        value,
    )?;
    execute_batch(session, vec![stmt]).await
}

/// Deletes an operational flag or timestamp from the `app_state` table.
#[allow(dead_code)]
pub async fn delete_app_state(
    db: &D1Database,
    session: &D1DatabaseSession,
    key: &str,
) -> Result<D1Metrics> {
    let stmt = query!(db, "DELETE FROM app_state WHERE key = ?1", key)?;
    execute_batch(session, vec![stmt]).await
}

/// Deletes multiple keys from the `app_state` table in a single atomic batch.
pub async fn delete_app_state_keys(
    db: &D1Database,
    session: &D1DatabaseSession,
    keys: &[&str],
) -> Result<D1Metrics> {
    let mut stmts = Vec::with_capacity(keys.len());
    for key in keys {
        stmts.push(query!(db, "DELETE FROM app_state WHERE key = ?1", *key)?);
    }
    execute_batch(session, stmts).await
}
