use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use worker::{
    Result,
    d1::{D1Database, D1DatabaseSession, D1PreparedArgument, D1PreparedStatement, D1Type},
    query,
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

/// M3: true emptiness check for the cold-start seed guard. Counts every
/// row in `notices`, not just the ones matching the current page-1 links —
/// a DB that still holds older rows is not empty even when none of them
/// appear on page 1 anymore.
pub async fn count_notices(session: &D1DatabaseSession) -> Result<i64> {
    #[derive(Deserialize)]
    struct CountRow {
        n: i64,
    }
    let stmt = session.prepare("SELECT COUNT(*) AS n FROM notices");
    let row: Option<CountRow> = stmt.first(None).await?;
    Ok(row.map(|r| r.n).unwrap_or(0))
}

/// Builds a parameterized upsert statement using `worker::query!` (`&D1Database`).
/// O5: `attachments_json` is computed once by the caller (`run()`),
/// not re-serialized here — it was previously computed once per call
/// site (upsert + silent-sync check) for the same notice.
pub fn build_upsert_statement(
    db: &D1Database,
    notice: &Notice,
    content_hash: &str,
    attachments_json: &str,
    detail_attempted: bool,
) -> Result<D1PreparedStatement> {
    let now = Utc::now().to_rfc3339();
    let pub_date = notice.published_date.as_deref().unwrap_or("");

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

/// Test-only SQL mirrors for the three [`build_upsert_statement`]
/// branches, so their semantics can be tested behaviourally against a
/// real SQLite database in tests (same pattern as
/// `lightweight_detail_update_sql()` above).
///
/// The production builder duplicates these SQL literals because the
/// `worker::query!` macro requires a string literal — keep the two in
/// sync if either changes.
///
/// - `(true, true)`: detail attempted and fetched — stores the fetched
///   body/attachments and sets `detail_fetched_at`.
/// - `(true, false)`: detail attempted but not fetched — stores empty
///   body/attachments and leaves `detail_fetched_at` NULL.
/// - `(false, _)`: detail not attempted — stores empty body/
///   attachments and leaves both detail timestamps NULL.
#[cfg(test)]
pub fn upsert_statement_sql(detail_attempted: bool, detail_fetched: bool) -> &'static str {
    if detail_attempted && detail_fetched {
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
            detail_fetched_at = excluded.detail_fetched_at"
    } else if detail_attempted {
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
            detail_checked_at = excluded.detail_checked_at"
    } else {
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
            last_seen_at = excluded.last_seen_at"
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
/// poisons the batch), fall back to bisection (O8) — retry halves until
/// single bad statements are isolated — log each per-statement failure,
/// and continue with the remaining chunks instead of aborting the whole
/// run / dispatch. Only if every statement in a chunk also fails
/// individually is the error propagated, so a single bad row cannot
/// silently discard the rest.
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
                    // Audit #9: a batch can resolve with a failed
                    // result inside instead of rejecting — the
                    // metadata alone does not prove the statement
                    // applied. Count it failed and log the error, or
                    // the partial-failure machinery is bypassed in
                    // exactly the failure mode it guards.
                    if !res.success() {
                        metrics.failed_statements += 1;
                        worker::console_warn!(
                            "[D1 Batch Partial Failure] Chunk {} statement reported success=false: {:?}; counting it as failed.",
                            chunk_idx,
                            res.error()
                        );
                    }
                }
            }
            Err(batch_err) => {
                worker::console_warn!(
                    "[D1 Batch Fallback] Chunk {} ({} statements) failed as a batch: {}; retrying by bisection.",
                    chunk_idx,
                    chunk.len(),
                    batch_err
                );
                let (fallback_metrics, succeeded) =
                    execute_statements_bisect(session, chunk, chunk_idx, 0).await;
                metrics.rows_read += fallback_metrics.rows_read;
                metrics.rows_written += fallback_metrics.rows_written;
                metrics.failed_statements += fallback_metrics.failed_statements;
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
                // M8: surface the per-statement failures to the caller
                // instead of swallowing them. A batch that returns Ok with
                // failed_statements > 0 only partially applied.
                worker::console_warn!(
                    "[D1 Batch Fallback] Chunk {} recovered: {}/{} statements applied ({} failed).",
                    chunk_idx,
                    succeeded,
                    chunk.len(),
                    fallback_metrics.failed_statements
                );
            }
        }
    }

    Ok(metrics)
}

/// O8: fallback for a chunk that failed as a batch. Returns
/// `(metrics, succeeded_count)`. The old code retried the chunk's
/// statements one at a time — up to 50 serial round-trips for a full
/// chunk. Bisection retries halves instead: a half that succeeds
/// applies in one call, a half that fails splits again, until single
/// bad statements are isolated. One bad statement in a 50-statement
/// chunk costs ~12 round-trips instead of 50.
///
/// `offset` is the index of `stmts[0]` within the chunk, for logging.
async fn execute_statements_bisect(
    session: &D1DatabaseSession,
    stmts: &[D1PreparedStatement],
    chunk_idx: usize,
    offset: usize,
) -> (D1Metrics, usize) {
    if stmts.is_empty() {
        return (D1Metrics::default(), 0);
    }
    // Note: `session.batch()` takes an owned Vec, so the slice is
    // cloned here. The clone is a set of cheap JS-handle refcount
    // bumps (not a deep copy), and the borrowed slice stays available
    // for the fallback — that is why execute_batch keeps the chunk
    // borrowed instead of draining owned statements into batch().
    match session.batch(stmts.to_vec()).await {
        Ok(results) => {
            let mut metrics = D1Metrics::default();
            let mut succeeded = 0;
            for (i, res) in results.iter().enumerate() {
                if let Ok(Some(meta)) = res.meta() {
                    metrics.rows_read += meta.rows_read.unwrap_or(0);
                    metrics.rows_written += meta.rows_written.unwrap_or(0);
                }
                // Audit #9: same check as in `execute_batch` — a
                // resolved batch can still contain a failed result,
                // which must not count as applied.
                if res.success() {
                    succeeded += 1;
                } else {
                    metrics.failed_statements += 1;
                    worker::console_warn!(
                        "[D1 Batch Fallback] Chunk {} statement {} reported success=false: {:?}; counting it as failed.",
                        chunk_idx,
                        offset + i,
                        res.error()
                    );
                }
            }
            (metrics, succeeded)
        }
        Err(err) => {
            if stmts.len() == 1 {
                worker::console_warn!(
                    "[D1 Batch Fallback] Chunk {} statement {} failed individually: {}",
                    chunk_idx,
                    offset,
                    err
                );
                (
                    D1Metrics {
                        failed_statements: 1,
                        ..Default::default()
                    },
                    0,
                )
            } else {
                let mid = stmts.len() / 2;
                let (mut m1, s1) = Box::pin(execute_statements_bisect(
                    session,
                    &stmts[..mid],
                    chunk_idx,
                    offset,
                ))
                .await;
                let (m2, s2) = Box::pin(execute_statements_bisect(
                    session,
                    &stmts[mid..],
                    chunk_idx,
                    offset + mid,
                ))
                .await;
                m1.rows_read += m2.rows_read;
                m1.rows_written += m2.rows_written;
                m1.failed_statements += m2.failed_statements;
                (m1, s1 + s2)
            }
        }
    }
}

// ── Telegram delivery tracking ───────────────────────────────────────

/// O1: builds an attempt-mark statement without executing it, so
/// dispatch can accumulate marks and flush one batch per platform
/// instead of one round-trip per send.
pub fn build_tg_attempt_statement(
    db: &D1Database,
    link: &str,
    now: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET tg_notification_attempted_at=?1 WHERE link=?2",
        now,
        link,
    )
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

// ── Facebook delivery tracking ───────────────────────────────────────

/// O1: builds an attempt-mark statement without executing it (see
/// `build_tg_attempt_statement`).
pub fn build_fb_attempt_statement(
    db: &D1Database,
    link: &str,
    now: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET fb_notification_attempted_at=?1 WHERE link=?2",
        now,
        link,
    )
}

/// O1: builds a Facebook notified-mark statement without executing it.
/// The SQL is identical to the inline statement `mark_fb_notified`
/// used to build.
pub fn build_fb_notified_statement(
    db: &D1Database,
    link: &str,
    content_hash: &str,
    post_id: &str,
    now: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET fb_sent_at=COALESCE(fb_sent_at, ?1), fb_last_notified_hash=?2, fb_post_id=?3 WHERE link=?4",
        now,
        content_hash,
        post_id,
        link,
    )
}

/// O1: builds a Facebook duplicate-handled statement without executing
/// it. The SQL is identical to `fb_duplicate_handled_sql()` — keep the
/// two in sync (see the note above that function).
pub fn build_fb_duplicate_handled_statement(
    db: &D1Database,
    link: &str,
    content_hash: &str,
    now: &str,
) -> Result<D1PreparedStatement> {
    query!(
        db,
        "UPDATE notices SET fb_sent_at=COALESCE(fb_sent_at, ?1), fb_last_notified_hash=?2, fb_notification_attempted_at=?3 WHERE link=?4",
        now,
        content_hash,
        now,
        link,
    )
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

/// O7: reads several operational keys in one round-trip instead of one
/// `get_app_state` call per key. The outage/recovery handlers and
/// `is_facebook_paused()` each made 2 sequential reads on every run
/// that hit those paths; one `IN (...)` query replaces them.
pub async fn get_app_states(
    session: &D1DatabaseSession,
    keys: &[&str],
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    if keys.is_empty() {
        return Ok(map);
    }
    #[derive(Deserialize)]
    struct KeyValueRow {
        key: String,
        value: String,
    }
    let d1_types: Vec<D1Type> = keys.iter().map(|k| D1Type::Text(k)).collect();
    let args: Vec<D1PreparedArgument> = d1_types.iter().map(D1PreparedArgument::new).collect();
    let stmt = session
        .prepare(app_states_in_sql(keys.len()))
        .bind_refs(&args)?;
    let result = stmt.all().await?;
    let rows: Vec<KeyValueRow> = result.results()?;
    for row in rows {
        map.insert(row.key, row.value);
    }
    Ok(map)
}

/// SQL for [`get_app_states`], extracted so its semantics can be
/// tested behaviourally against a real SQLite database in tests (same
/// pattern as `lightweight_detail_update_sql()`).
pub fn app_states_in_sql(count: usize) -> String {
    let placeholders: Vec<String> = (1..=count).map(|i| format!("?{i}")).collect();
    format!(
        "SELECT key, value FROM app_state WHERE key IN ({})",
        placeholders.join(", ")
    )
}
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
