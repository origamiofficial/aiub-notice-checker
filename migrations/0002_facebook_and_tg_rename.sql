-- Migration 0002: Add Facebook tracking columns, rename Telegram columns, and backfill.

-- 1. Rename existing Telegram columns to tg_ prefix
ALTER TABLE notices RENAME COLUMN sent_at TO tg_sent_at;
ALTER TABLE notices RENAME COLUMN last_notified_hash TO tg_last_notified_hash;
ALTER TABLE notices RENAME COLUMN notification_attempted_at TO tg_notification_attempted_at;

-- 2. Add dedicated Facebook tracking columns (nullable text)
ALTER TABLE notices ADD COLUMN fb_sent_at TEXT;
ALTER TABLE notices ADD COLUMN fb_last_notified_hash TEXT;
ALTER TABLE notices ADD COLUMN fb_notification_attempted_at TEXT;
ALTER TABLE notices ADD COLUMN fb_post_id TEXT;

-- 3. Backfill Facebook columns for all existing rows already sent via Telegram.
-- This prevents the historical flood trap where hundreds of archived notices
-- would otherwise be blasted to Facebook on the first deployment run.
UPDATE notices
SET fb_sent_at = tg_sent_at,
    fb_last_notified_hash = tg_last_notified_hash
WHERE tg_sent_at IS NOT NULL;
