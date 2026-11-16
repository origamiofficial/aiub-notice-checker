use chrono::Utc;
use worker::d1::D1Database;
use worker::wasm_bindgen::JsValue;
use worker::Result;

use crate::models::{Notice, NoticeRow};

pub fn utc_now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

fn opt_str_param(val: Option<&str>) -> JsValue {
    match val {
        Some(s) => JsValue::from_str(s),
        None => JsValue::NULL,
    }
}

pub async fn get_notices_by_links(db: &D1Database, links: &[String]) -> Result<Vec<NoticeRow>> {
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=links.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT title, description, link, published_date, body_text, attachments_json, \
         content_hash, sent_at, last_notified_hash, detail_checked_at, detail_fetched_at \
         FROM notices WHERE link IN ({placeholders})"
    );
    let params: Vec<JsValue> = links.iter().map(|l| JsValue::from_str(l)).collect();
    let result = db.prepare(&sql).bind(&params)?.all().await?;
    result.results::<NoticeRow>()
}

pub async fn get_pending_detail_rows(db: &D1Database, limit: usize) -> Result<Vec<NoticeRow>> {
    let sql = "SELECT title, description, link, published_date, body_text, attachments_json, \
               content_hash, sent_at, last_notified_hash, detail_checked_at, detail_fetched_at \
               FROM notices WHERE detail_fetched_at IS NULL \
               ORDER BY COALESCE(detail_checked_at, '') ASC, first_seen_at DESC LIMIT ?1";
    let result = db
        .prepare(sql)
        .bind(&[JsValue::from_f64(limit as f64)])?
        .all()
        .await?;
    result.results::<NoticeRow>()
}

pub async fn get_unsent_retry_rows(db: &D1Database, limit: usize) -> Result<Vec<NoticeRow>> {
    let sql = "SELECT title, description, link, published_date, body_text, attachments_json, \
               content_hash, sent_at, last_notified_hash, detail_checked_at, detail_fetched_at \
               FROM notices WHERE sent_at IS NULL OR last_notified_hash IS NULL \
               OR last_notified_hash != content_hash \
               ORDER BY notification_attempted_at IS NOT NULL ASC, \
               COALESCE(notification_attempted_at, content_changed_at) ASC LIMIT ?1";
    let result = db
        .prepare(sql)
        .bind(&[JsValue::from_f64(limit as f64)])?
        .all()
        .await?;
    result.results::<NoticeRow>()
}

pub async fn upsert_notice(
    db: &D1Database,
    notice: &Notice,
    new_hash: &str,
    existing: Option<&NoticeRow>,
    detail_checked: bool,
) -> Result<()> {
    let now = utc_now_iso();
    let attachments_json = notice.attachments_json();

    match existing {
        None => {
            let sql = "INSERT INTO notices \
                (title, description, link, published_date, body_text, attachments_json, \
                 content_hash, first_seen_at, last_seen_at, content_changed_at, \
                 detail_checked_at, detail_fetched_at) \
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";
            let checked_ts = detail_checked.then_some(now.as_str());
            let fetched_ts = notice.detail_fetched.then_some(now.as_str());
            db.prepare(sql)
                .bind(&[
                    JsValue::from_str(&notice.title),
                    JsValue::from_str(&notice.description),
                    JsValue::from_str(&notice.url),
                    opt_str_param(notice.published_date.as_deref()),
                    JsValue::from_str(&notice.body_text),
                    JsValue::from_str(&attachments_json),
                    JsValue::from_str(new_hash),
                    JsValue::from_str(&now),
                    JsValue::from_str(&now),
                    JsValue::from_str(&now),
                    opt_str_param(checked_ts),
                    opt_str_param(fetched_ts),
                ])?
                .run()
                .await?;
        }
        Some(prev) if prev.content_hash != new_hash => {
            let sql = "UPDATE notices SET title=?1, description=?2, published_date=?3, \
                body_text=?4, attachments_json=?5, content_hash=?6, last_seen_at=?7, \
                content_changed_at=?8, notification_attempted_at=NULL, \
                detail_checked_at=COALESCE(?9, detail_checked_at), \
                detail_fetched_at=COALESCE(?10, detail_fetched_at) WHERE link=?11";
            let checked_ts = detail_checked.then_some(now.as_str());
            let fetched_ts = notice.detail_fetched.then_some(now.as_str());
            db.prepare(sql)
                .bind(&[
                    JsValue::from_str(&notice.title),
                    JsValue::from_str(&notice.description),
                    opt_str_param(notice.published_date.as_deref()),
                    JsValue::from_str(&notice.body_text),
                    JsValue::from_str(&attachments_json),
                    JsValue::from_str(new_hash),
                    JsValue::from_str(&now),
                    JsValue::from_str(&now),
                    opt_str_param(checked_ts),
                    opt_str_param(fetched_ts),
                    JsValue::from_str(&notice.url),
                ])?
                .run()
                .await?;
        }
        Some(_) if detail_checked => {
            let sql = "UPDATE notices SET detail_checked_at=?1, \
                detail_fetched_at=COALESCE(?2, detail_fetched_at) WHERE link=?3";
            let fetched_ts = notice.detail_fetched.then_some(now.as_str());
            db.prepare(sql)
                .bind(&[
                    JsValue::from_str(&now),
                    opt_str_param(fetched_ts),
                    JsValue::from_str(&notice.url),
                ])?
                .run()
                .await?;
        }
        Some(_) => {}
    }

    Ok(())
}

pub async fn mark_notification_attempt(db: &D1Database, link: &str) -> Result<()> {
    let now = utc_now_iso();
    db.prepare("UPDATE notices SET notification_attempted_at=?1 WHERE link=?2")
        .bind(&[JsValue::from_str(&now), JsValue::from_str(link)])?
        .run()
        .await?;
    Ok(())
}

pub async fn mark_notified(db: &D1Database, link: &str, content_hash: &str) -> Result<()> {
    let now = utc_now_iso();
    db.prepare(
        "UPDATE notices SET sent_at=COALESCE(sent_at, ?1), last_notified_hash=?2 WHERE link=?3",
    )
    .bind(&[
        JsValue::from_str(&now),
        JsValue::from_str(content_hash),
        JsValue::from_str(link),
    ])?
    .run()
    .await?;
    Ok(())
}