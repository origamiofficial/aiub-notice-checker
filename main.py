from __future__ import annotations

import datetime as dt
import hashlib
import html as html_lib
import json
import logging
import os
import re
import sqlite3
import tempfile
from dataclasses import dataclass
from email.utils import format_datetime
from typing import Mapping
from urllib.parse import urldefrag, urljoin, urlparse

import requests
from lxml import etree
from lxml import html as lxml_html
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry
import xml.etree.ElementTree as ET


BASE_URL = "https://www.aiub.edu"
BASE_HOST = urlparse(BASE_URL).netloc.lower()
NOTICE_PATH = "/category/notices"

LIST_PAGE_SIZE = 100
MAX_LIST_PAGES = 60
MIN_SCAN_PAGES = 2
REQUEST_TIMEOUT = (5.0, 30.0)
DETAIL_FETCH_LIMIT = LIST_PAGE_SIZE
DRY_RUN = os.environ.get("DRY_RUN", "false").lower() in {"1", "true", "yes"}
TELEGRAM_MAX_RETRIES = 3
TELEGRAM_RETRY_BACKOFF = 2.0

DB_NAME = "aiub_notices.db"
DB_TIMEOUT = 30.0
RSS_FEED_FILE = "rss.xml"
RSS_ITEM_LIMIT = 500
SCRIPT_VERSION = "5.0"

POST_XPATH = (
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' notification ') "
    "and not(ancestor::div[contains(concat(' ', normalize-space(@class), ' '), ' notification ')])]"
)
TITLE_XPATH = ".//h2[contains(concat(' ', normalize-space(@class), ' '), ' title ')]/text()"
LINK_XPATH = ".//a[contains(concat(' ', normalize-space(@class), ' '), ' info-link ')]/@href"
DESCRIPTION_XPATH = ".//p[contains(concat(' ', normalize-space(@class), ' '), ' desc ')]/text()"
DATE_TEXT_XPATH = ".//div[contains(concat(' ', normalize-space(@class), ' '), ' date-custom ')]//text()[normalize-space()]"
DETAIL_TITLE_XPATH = "string(//h1[@id='dynamicHeading'])"
DETAIL_BODY_XPATH = (
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' notice-page ')]"
    "//div[contains(concat(' ', normalize-space(@class), ' '), ' question-column ') "
    "and not(contains(concat(' ', normalize-space(@class), ' '), ' notice-sticky-header '))]"
)

@dataclass
class Notice:
    title: str
    description: str
    url: str
    published_date: str | None
    body_text: str = ""
    attachments: list[str] | None = None

    @property
    def attachments_json(self) -> str:
        return json.dumps(self.attachments or [], ensure_ascii=False, sort_keys=True)


def configure_logging() -> None:
    logging.basicConfig(
        level=os.environ.get("LOG_LEVEL", "INFO"),
        format="%(asctime)s %(levelname)s %(message)s",
    )


def clean_text(text: str) -> str:
    return re.sub(r"\s+", " ", html_lib.unescape(str(text))).strip()


def normalize_url(href: str) -> str:
    href = (href or "").replace("\\", "/").strip()
    if not href:
        return ""
    absolute = urljoin(f"{BASE_URL}/", href)
    absolute, _fragment = urldefrag(absolute)
    parsed = urlparse(absolute)
    if parsed.scheme not in {"http", "https"}:
        logging.warning("Ignoring URL with unsupported scheme: %r", href)
        return ""
    if parsed.netloc.lower() != BASE_HOST:
        logging.warning("Ignoring external URL: %s", absolute)
        return ""
    return absolute


def parse_date_parts(parts: list[str]) -> str | None:
    text = re.sub(r"\bSept\b", "Sep", clean_text(" ".join(parts)), flags=re.IGNORECASE)
    if not text:
        return None

    for date_format in ("%d %b %Y", "%d %B %Y"):
        try:
            return dt.datetime.strptime(text, date_format).date().isoformat()
        except ValueError:
            pass
    logging.warning("Could not parse notice date from parts: %s", text)
    return None


def display_date(iso_date: str | None) -> str:
    if not iso_date:
        return "Unknown"
    try:
        return dt.date.fromisoformat(iso_date).strftime("%d %b %Y")
    except ValueError:
        return iso_date


def create_aiub_session() -> requests.Session:
    session = requests.Session()
    session.headers.update(
        {
            "User-Agent": "aiub-notice-checker/5.0 (+https://github.com/origamiofficial/aiub-notice-checker)",
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        }
    )
    retry_options = {
        "total": 4,
        "connect": 4,
        "read": 4,
        "status": 4,
        "backoff_factor": 1.0,
        "status_forcelist": (429, 500, 502, 503, 504),
        "respect_retry_after_header": True,
    }
    retry = Retry(allowed_methods=frozenset(["GET"]), **retry_options)
    adapter = HTTPAdapter(max_retries=retry, pool_connections=10, pool_maxsize=10)
    session.mount("https://", adapter)
    session.mount("http://", adapter)
    return session


def create_telegram_session() -> requests.Session:
    session = requests.Session()
    session.headers.update({"User-Agent": "aiub-notice-checker/5.0"})
    retry = Retry(
        total=max(0, TELEGRAM_MAX_RETRIES - 1),
        status=TELEGRAM_MAX_RETRIES,
        backoff_factor=TELEGRAM_RETRY_BACKOFF,
        status_forcelist=(429, 500, 502, 503, 504),
        allowed_methods=frozenset(["POST"]),
        respect_retry_after_header=True,
        raise_on_status=False,
    )
    adapter = HTTPAdapter(max_retries=retry)
    session.mount("https://", adapter)
    session.mount("http://", adapter)
    return session


def fetch_html(session: requests.Session, url: str) -> lxml_html.HtmlElement:
    response = session.get(url, timeout=REQUEST_TIMEOUT)
    response.raise_for_status()
    return lxml_html.fromstring(response.content)


def parse_listing_page(tree: lxml_html.HtmlElement) -> list[Notice]:
    notices: list[Notice] = []
    for post in tree.xpath(POST_XPATH):
        title = clean_text("".join(post.xpath(TITLE_XPATH)))
        description = clean_text("".join(post.xpath(DESCRIPTION_XPATH)))
        link = "".join(post.xpath(LINK_XPATH)).strip()
        url = normalize_url(link)
        date_parts = [text for part in post.xpath(DATE_TEXT_XPATH) if (text := clean_text(part))]

        if not url:
            logging.warning("Skipping notice without a link: %s", title)
            continue

        notices.append(
            Notice(
                title=title,
                description=description,
                url=url,
                published_date=parse_date_parts(date_parts),
                attachments=[],
            )
        )
    return notices


def parse_detail_page(tree: lxml_html.HtmlElement, fallback: Notice) -> Notice:
    title = clean_text(tree.xpath(DETAIL_TITLE_XPATH)) or fallback.title
    body_nodes = tree.xpath(DETAIL_BODY_XPATH)
    body_parts = []
    for node in body_nodes:
        text = clean_text(node.text_content())
        if text:
            body_parts.append(text)
    body_text = "\n\n".join(body_parts)

    attachments: list[str] = []
    for node in body_nodes:
        for href in node.xpath(".//a[@href]/@href"):
            normalized = normalize_url(href)
            if normalized and normalized not in attachments:
                attachments.append(normalized)

    return Notice(
        title=title,
        description=fallback.description,
        url=fallback.url,
        published_date=fallback.published_date,
        body_text=body_text,
        attachments=attachments,
    )


def enrich_notice(session: requests.Session, notice: Notice) -> Notice:
    try:
        tree = fetch_html(session, notice.url)
        return parse_detail_page(tree, notice)
    except (requests.RequestException, etree.ParserError, ValueError) as exc:
        logging.warning("Could not fetch notice detail %s: %s", notice.url, exc)
        return notice


def crawl_notices(session: requests.Session, known_links: set[str]) -> list[Notice]:
    notices: list[Notice] = []
    seen_links: set[str] = set()
    seen_page_fingerprints: set[tuple[str, ...]] = set()

    for page_no in range(1, MAX_LIST_PAGES + 1):
        url = f"{BASE_URL}{NOTICE_PATH}?pageNo={page_no}&pageSize={LIST_PAGE_SIZE}"
        logging.info("Fetching listing page %s", page_no)
        tree = fetch_html(session, url)
        page_notices = parse_listing_page(tree)

        if not page_notices:
            logging.info("Stopping at page %s because it returned no notices.", page_no)
            break

        fingerprint = tuple(notice.url for notice in page_notices)
        if fingerprint in seen_page_fingerprints:
            logging.info("Stopping at page %s because the page repeated a previous result.", page_no)
            break
        seen_page_fingerprints.add(fingerprint)

        page_has_new_link = False
        for notice in page_notices:
            if notice.url in seen_links:
                continue
            seen_links.add(notice.url)
            notices.append(notice)
            if notice.url not in known_links:
                page_has_new_link = True

        if known_links and page_no >= MIN_SCAN_PAGES and not page_has_new_link:
            logging.info("Stopping at page %s because all links on the page are already known.", page_no)
            break

    return notices


def connect_db() -> sqlite3.Connection:
    conn = sqlite3.connect(DB_NAME, timeout=DB_TIMEOUT)
    conn.row_factory = sqlite3.Row
    conn.execute(f"PRAGMA busy_timeout={int(DB_TIMEOUT * 1000)}")
    return conn


def ensure_schema(conn: sqlite3.Connection) -> None:
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS notices (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            link TEXT NOT NULL,
            published_date TEXT,
            body_text TEXT NOT NULL DEFAULT '',
            attachments_json TEXT NOT NULL DEFAULT '[]',
            content_hash TEXT NOT NULL DEFAULT '',
            first_seen_at TEXT NOT NULL DEFAULT '',
            last_seen_at TEXT NOT NULL DEFAULT '',
            sent_at TEXT,
            last_notified_hash TEXT
        )
        """
    )
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_notices_link ON notices(link)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_notices_published_date ON notices(published_date)")
    conn.commit()


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


def compute_hash(
    title: str,
    description: str,
    link: str,
    published_date: str | None,
    body_text: str,
    attachments_json: str,
) -> str:
    payload = json.dumps(
        {
            "title": clean_text(title),
            "description": clean_text(description),
            "link": normalize_url(link),
            "published_date": published_date or "",
            "body_text": clean_text(body_text),
            "attachments": parse_attachments_json(attachments_json),
        },
        ensure_ascii=False,
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def notice_hash(notice: Notice) -> str:
    return compute_hash(
        notice.title,
        notice.description,
        notice.url,
        notice.published_date,
        notice.body_text,
        notice.attachments_json,
    )


def load_existing_notice_rows(conn: sqlite3.Connection) -> dict[str, sqlite3.Row]:
    return {
        link: row
        for row in conn.execute("SELECT * FROM notices")
        if (link := normalize_url(row["link"]))
    }


def get_notice_row(conn: sqlite3.Connection, url: str) -> sqlite3.Row | None:
    return conn.execute("SELECT * FROM notices WHERE link=?", (normalize_url(url),)).fetchone()


def parse_attachments_json(value: str | None) -> list[str]:
    if not value:
        return []
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        logging.warning("Ignoring invalid attachments_json value: %r", value)
        return []
    if not isinstance(parsed, list):
        return []
    return [str(item) for item in parsed]


def merge_cached_notice(listing_notice: Notice, existing: sqlite3.Row | None) -> Notice:
    if existing is None:
        return listing_notice

    cached_body = existing["body_text"] or ""
    cached_attachments = parse_attachments_json(existing["attachments_json"])
    cached_title = existing["title"] or ""
    title = listing_notice.title
    if cached_title and (
        cached_body
        or cached_attachments
        or ("..." in listing_notice.title and "..." not in cached_title)
    ):
        title = cached_title

    return Notice(
        title=title,
        description=listing_notice.description or existing["description"] or "",
        url=listing_notice.url,
        published_date=listing_notice.published_date or existing["published_date"],
        body_text=cached_body,
        attachments=cached_attachments,
    )


def upsert_seen_notice(
    conn: sqlite3.Connection,
    notice: Notice,
    content_hash: str,
) -> None:
    now = utc_now()
    conn.execute(
        """
        INSERT INTO notices
            (title, description, link, published_date, body_text, attachments_json,
             content_hash, first_seen_at, last_seen_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(link) DO UPDATE SET
            title=excluded.title,
            description=excluded.description,
            published_date=excluded.published_date,
            body_text=excluded.body_text,
            attachments_json=excluded.attachments_json,
            content_hash=excluded.content_hash,
            last_seen_at=excluded.last_seen_at
        """,
        (
            notice.title,
            notice.description,
            notice.url,
            notice.published_date,
            notice.body_text,
            notice.attachments_json,
            content_hash,
            now,
            now,
        ),
    )


def mark_notified(conn: sqlite3.Connection, notice_url: str, content_hash: str) -> None:
    now = utc_now()
    conn.execute(
        """
        UPDATE notices
        SET sent_at=COALESCE(sent_at, ?),
            last_notified_hash=?
        WHERE link=?
        """,
        (now, content_hash, normalize_url(notice_url)),
    )
    conn.commit()


def seed_without_notification(conn: sqlite3.Connection, notice_url: str, content_hash: str) -> None:
    now = utc_now()
    conn.execute(
        """
        UPDATE notices
        SET sent_at=COALESCE(sent_at, ?),
            last_notified_hash=?
        WHERE link=?
        """,
        (now, content_hash, normalize_url(notice_url)),
    )


def truncate_text(text: str, limit: int) -> str:
    text = str(text).strip()
    if len(text) <= limit:
        return text
    return text[: limit - 3].rstrip() + "..."


def format_notice_message(notice: Notice, gh_run_no: str, edited: bool = False) -> str:
    description = notice.description or first_nonempty_line(notice.body_text) or "Please click the link for details."
    message = (
        f"{'[EDITED] ' if edited else ''}{truncate_text(notice.title, 500)}\n\n"
        f"Date: {display_date(notice.published_date)}\n\n"
        f"{truncate_text(description, 1600)}\n\n"
        f"{notice.url}#{gh_run_no}"
    )
    return truncate_text(message, 4096)


def first_nonempty_line(text: str) -> str:
    for line in text.splitlines():
        line = clean_text(line)
        if line:
            return line
    return ""


def send_telegram_message(
    session: requests.Session,
    chat_id: str,
    message: str,
    config: dict[str, str | None],
    label: str,
) -> bool:
    if DRY_RUN:
        logging.info("DRY_RUN: would send %s to Telegram chat %s", label, chat_id)
        return True

    bot_api_key = config["bot_api_key"]
    url = f"https://api.telegram.org/bot{bot_api_key}/sendMessage"
    payload = {
        "chat_id": chat_id,
        "text": message,
        "disable_web_page_preview": True,
    }

    try:
        response = session.post(url, json=payload, timeout=REQUEST_TIMEOUT)
    except requests.RequestException as exc:
        logging.error("Telegram send failed for %s: %s", label, exc)
        return False

    if response.status_code == 200:
        logging.info("Sent %s to Telegram.", label)
        return True

    logging.error("Telegram send failed for %s: %s", label, response.text)
    return False


def should_fetch_detail(existing: sqlite3.Row | None, index: int) -> bool:
    return index < DETAIL_FETCH_LIMIT and (existing is None or not existing["body_text"])


def process_notices(
    conn: sqlite3.Connection,
    aiub_session: requests.Session,
    telegram_session: requests.Session,
    notices: list[Notice],
    config: dict[str, str | None],
    first_run: bool,
    existing_by_url: Mapping[str, sqlite3.Row] | None = None,
) -> tuple[int, int, int]:
    new_count = 0
    edited_count = 0
    failed_notifications = 0
    chat_id = str(config["chat_id"])
    gh_run_no = str(config["github_run_number"])
    existing_by_url = existing_by_url or load_existing_notice_rows(conn)

    detail_targets = [
        listing_notice
        for index, listing_notice in enumerate(notices)
        if should_fetch_detail(existing_by_url.get(listing_notice.url), index)
    ]
    enriched_by_url = {notice.url: enrich_notice(aiub_session, notice) for notice in detail_targets}

    for index, listing_notice in enumerate(notices):
        existing = existing_by_url.get(listing_notice.url)
        if should_fetch_detail(existing, index):
            notice = enriched_by_url.get(listing_notice.url, listing_notice)
            if existing is not None and not notice.body_text:
                notice = merge_cached_notice(notice, existing)
        else:
            notice = merge_cached_notice(listing_notice, existing)
        new_hash = notice_hash(notice)
        previous_hash = existing["content_hash"] if existing else None
        previous_notified_hash = existing["last_notified_hash"] if existing else None
        had_detail_before = bool(existing and existing["body_text"])

        upsert_seen_notice(conn, notice, new_hash)

        if existing is None:
            new_count += 1
            if first_run:
                seed_without_notification(conn, notice.url, new_hash)
                logging.info("Seeded existing notice without Telegram notification: %s", notice.title)
                continue

            message = format_notice_message(notice, gh_run_no, edited=False)
            if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
                mark_notified(conn, notice.url, new_hash)
            else:
                failed_notifications += 1
            continue

        if existing["sent_at"] is None:
            message = format_notice_message(notice, gh_run_no, edited=False)
            if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
                mark_notified(conn, notice.url, new_hash)
            else:
                failed_notifications += 1
            continue

        if previous_hash and previous_hash != new_hash:
            if not had_detail_before:
                seed_without_notification(conn, notice.url, new_hash)
                logging.info("Backfilled detail data without edit notification: %s", notice.title)
                continue

            if previous_notified_hash != new_hash:
                edited_count += 1
                message = format_notice_message(notice, gh_run_no, edited=True)
                if send_telegram_message(telegram_session, chat_id, message, config, notice.title):
                    mark_notified(conn, notice.url, new_hash)
                else:
                    failed_notifications += 1

    conn.commit()
    return new_count, edited_count, failed_notifications


def generate_rss_feed(conn: sqlite3.Connection) -> None:
    rows = conn.execute(
        """
        SELECT title, description, link, published_date, body_text
        FROM notices
        ORDER BY COALESCE(published_date, '') DESC, id DESC
        LIMIT ?
        """,
        (RSS_ITEM_LIMIT,),
    ).fetchall()

    ET.register_namespace("atom", "http://www.w3.org/2005/Atom")
    rss = ET.Element("rss", version="2.0")
    channel = ET.SubElement(rss, "channel")
    ET.SubElement(channel, "title").text = "AIUB Notices"
    ET.SubElement(channel, "link").text = f"{BASE_URL}{NOTICE_PATH}"
    ET.SubElement(channel, "description").text = "Latest notices from AIUB."

    self_link = ET.SubElement(channel, "{http://www.w3.org/2005/Atom}link")
    self_link.set("rel", "self")
    self_link.set("type", "application/rss+xml")
    self_link.set("href", "https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/rss.xml")

    for row in rows:
        link = normalize_url(row["link"])
        if not link:
            logging.warning("Skipping RSS item with invalid link: %r", row["link"])
            continue
        item = ET.SubElement(channel, "item")
        ET.SubElement(item, "title").text = row["title"]
        ET.SubElement(item, "description").text = row["description"] or first_nonempty_line(row["body_text"])
        ET.SubElement(item, "link").text = link
        ET.SubElement(item, "guid").text = link
        pub_date = rss_pub_date(row["published_date"])
        if pub_date:
            ET.SubElement(item, "pubDate").text = pub_date

    tree = ET.ElementTree(rss)
    feed_dir = os.path.dirname(os.path.abspath(RSS_FEED_FILE)) or "."
    os.makedirs(feed_dir, exist_ok=True)
    temp_path = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            delete=False,
            dir=feed_dir,
            prefix=f".{os.path.basename(RSS_FEED_FILE)}.",
            suffix=".tmp",
        ) as temp_file:
            temp_path = temp_file.name
            tree.write(temp_file, encoding="UTF-8", xml_declaration=True, method="xml")
        os.replace(temp_path, RSS_FEED_FILE)
    finally:
        if temp_path and os.path.exists(temp_path):
            os.unlink(temp_path)
    logging.info("RSS feed generated at %s", RSS_FEED_FILE)


def rss_pub_date(iso_date: str | None) -> str | None:
    if not iso_date:
        return None
    try:
        date_value = dt.date.fromisoformat(iso_date)
        datetime_value = dt.datetime.combine(date_value, dt.time(), dt.timezone.utc)
        return format_datetime(datetime_value, usegmt=True)
    except ValueError:
        return None


def load_config() -> dict[str, str | None]:
    bot_api_key = os.environ.get("TELEGRAM_BOT_API_KEY")
    chat_id = os.environ.get("TELEGRAM_CHAT_ID")
    if not DRY_RUN and (not bot_api_key or not chat_id):
        raise RuntimeError("TELEGRAM_BOT_API_KEY and TELEGRAM_CHAT_ID are required unless DRY_RUN=true.")

    return {
        "bot_api_key": bot_api_key or "dry-run",
        "chat_id": chat_id or "dry-run",
        "admin_chat_id": os.environ.get("TELEGRAM_ADMIN_CHAT_ID"),
        "github_run_number": os.environ.get("GITHUB_RUN_NUMBER", "local"),
    }


def main() -> int:
    configure_logging()
    logging.info("AIUB notice checker v%s starting.", SCRIPT_VERSION)
    config = load_config()
    aiub_session = create_aiub_session()
    telegram_session = create_telegram_session()

    with connect_db() as conn:
        ensure_schema(conn)
        first_run = conn.execute("SELECT COUNT(*) FROM notices").fetchone()[0] == 0
        existing_by_url = load_existing_notice_rows(conn)
        known_links = set(existing_by_url)

        notices = crawl_notices(aiub_session, known_links)
        if not notices:
            admin_chat_id = config.get("admin_chat_id")
            if admin_chat_id:
                send_telegram_message(
                    telegram_session,
                    str(admin_chat_id),
                    (
                        "AIUB Notice\n\nNo notices were found on the listing page. "
                        "The page structure or parser may need to be updated."
                    ),
                    config,
                    "admin notification",
                )
            raise RuntimeError("No notices were collected from AIUB.")

        logging.info("Collected %s unique notices from listing pages.", len(notices))
        new_count, edited_count, failed_notifications = process_notices(
            conn,
            aiub_session,
            telegram_session,
            notices,
            config,
            first_run,
            existing_by_url,
        )
        generate_rss_feed(conn)

    logging.info(
        "Script completed. New=%s, Edited=%s, Failed notifications=%s",
        new_count,
        edited_count,
        failed_notifications,
    )
    return 1 if failed_notifications else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        logging.exception("Script failed: %s", exc)
        raise SystemExit(1)
