# AIUB Notice Checker

[![AIUB Notice Checker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml)
[![Telegram](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Telegram.svg)](https://t.me/aiubnotice)

Monitors the [AIUB notices page](https://www.aiub.edu/category/notices/), saves notices in SQLite, sends new and edited notices to Telegram, and publishes an [RSS feed](https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/rss.xml).

The GitHub Actions workflow requests a run every five minutes. [Scheduled runs can be delayed or dropped by GitHub](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule), so delivery time is not guaranteed.

## How it works

- The first validated scan of an empty database seeds the archive without announcing old notices.
- Later scans detect new listings and revisit detail pages in bounded batches to find edits, including attachment changes. Periodic full listing scans catch older or backdated additions.
- A failed Telegram send remains pending for a later run. Each run attempts at most 20 deliveries and honors Telegram rate limits. Delivery is at least once: a lost response or failed state commit can cause a duplicate message.
- Listing, pagination, and sampled detail-page validation failures fail the run; an optional admin chat receives source-health alerts.
- Telegram notice links include the GitHub Actions run number as a URL fragment so each message can be traced to the run that posted it.
- The workflow commits only `aiub_notices.db` and `rss.xml`. Concurrent checker runs are serialized, and a rejected Git push fails the job rather than overwriting repository history.
- Production executes `main.py` and its dependencies from the latest stable GitHub release. Changes on `main` remain a playground until compile checks, unit tests, and a live AIUB smoke test pass.
- The RSS feed contains up to 500 recently discovered or changed notices. Item IDs stay stable, though individual readers decide whether to display an edit as new.

## Stable releases

Pushes that change `main.py` or `requirements.txt` run the complete test workflow. After every check passes, the workflow increments the latest numeric release by `0.1`, validates the exact versioned source against the live AIUB site, creates an annotated tag, and publishes `main.py`, `requirements.txt`, and their SHA-256 checksums as the latest release.

If any check fails, no tag or release is created. Scheduled notice checks continue using the previous latest release and never execute the failing playground copy.

## Run locally

Use Python 3.13 and install the pinned dependencies:

```bash
python -m pip install -r requirements.txt
```

Set `TELEGRAM_BOT_API_KEY` and `TELEGRAM_CHAT_ID` for live notifications. `TELEGRAM_ADMIN_CHAT_ID` is optional and receives source error alerts. Then run:

```bash
python main.py
```

For a preview without Telegram delivery or changes to the tracked database and RSS file, set `DRY_RUN=true`:

```bash
DRY_RUN=true python main.py
```

Run the offline checks with:

```bash
python -m unittest discover -v
```

## RSS

Subscribe with any RSS reader:

```text
https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/rss.xml
```

## Contributing

Pull requests for parser fixes and other improvements are welcome. Pull requests run offline validation without publishing a release; stable releases are created only after tested runtime changes reach `main`.
