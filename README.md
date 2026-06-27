# AIUB Notice Checker

[![Facebook](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Facebook.svg)](https://facebook.com/aiubnotice)
[![Telegram](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Telegram.svg)](https://t.me/aiubnotice)
[![Twitter](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Twitter.svg)](https://twitter.com/aiubnotice)
[![LinkedIn](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/LinkedIN.svg)](https://linkedin.com/in/aiubnotice)
[![Discord](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Discord.svg)](https://discord.gg/M8XVrA2Fnb)

[![AIUB Notice Checker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/origamiofficial/aiub-notice-checker)
![We Support](https://img.shields.io/badge/we%20stand%20with-%F0%9F%87%B5%F0%9F%87%B8%20palestine-white.svg)
[![Hits](https://hits.seeyoufarm.com/api/count/incr/badge.svg?url=https://github.com/origamiofficial/aiub-notice-checker&icon=github.svg&icon_color=%23FFFFFF&title=hits&edge_flat=false)](https://github.com/origamiofficial/aiub-notice-checker)

A Python script that monitors the [AIUB Notice page](https://www.aiub.edu/category/notices/) for new or edited posts and instantly sends updates to a specified Telegram channel.

---

## 📡 RSS Feed

Subscribe to get notices in any RSS reader:

[![Valid RSS](https://validator.w3.org/feed/images/valid-rss-rogers.png)](http://validator.w3.org/feed/check.cgi?url=https%3A//github.com/origamiofficial/aiub-notice-checker/raw/main/rss.xml)

```
https://github.com/origamiofficial/aiub-notice-checker/raw/main/rss.xml
```

---

## ✨ Features

| Feature | Description |
|---|---|
| 🔄 Auto-update | Checks for script updates on every run and updates itself automatically |
| 🌐 Site health check | Verifies AIUB website is accessible before scraping |
| 🔍 XPath validation | Detects if the page structure has changed and alerts when XPaths need fixing |
| 🗄️ SQLite database | Stores all past notices locally to detect both new posts and edits |
| 📨 Telegram notifications | Sends formatted messages to your channel via the Telegram Bot API |
| 📰 RSS generation | Auto-generates an RSS 2.0 feed from the database after every run |

---

## 📋 Requirements

- Python 3.6 or higher
- `requests` library
- `lxml` library
- The following environment variables set with valid values:

| Variable | Purpose |
|---|---|
| `TELEGRAM_CHAT_ID` | The channel where notices are sent |
| `TELEGRAM_ADMIN_CHAT_ID` | Admin chat for error and debug alerts |
| `TELEGRAM_BOT_API_KEY` | Your Telegram bot token |
| `GITHUB_RUN_NUMBER` | Used internally for version tracking |

---

## 🚀 Setup & Usage

**1. Clone the repository**
```bash
git clone https://github.com/origamiofficial/aiub-notice-checker
cd aiub-notice-checker
```

**2. Install dependencies**
```bash
pip install -r requirements.txt
```

**3. Set your environment variables**
```bash
export TELEGRAM_CHAT_ID=your_channel_id
export TELEGRAM_ADMIN_CHAT_ID=your_admin_chat_id
export TELEGRAM_BOT_API_KEY=your_bot_token
export GITHUB_RUN_NUMBER=1
```

**4. Run the script**
```bash
python main.py
```

---

## ⚙️ How It Works

```
Start
  │
  ├─ 1. Check for script updates → auto-replace if newer version found
  │
  ├─ 2. Ping AIUB website → exit gracefully if unreachable
  │
  ├─ 3. Validate XPath expressions → alert admin if site structure changed
  │
  ├─ 4. Connect to SQLite database → create new DB if first run
  │
  ├─ 5. Scrape all notices from the AIUB Notice page
  │
  ├─ 6. Compare each notice against database
  │       ├─ New post?    → flag for Telegram notification
  │       └─ Edited post? → flag for Telegram notification
  │
  ├─ 7. Rebuild RSS feed from updated database
  │
  ├─ 8. Send Telegram messages for all new/edited notices
  │
  └─ 9. Save updated state to database → close connection
```

---

## 🤝 Contribution

If AIUB updates their website and breaks the scraper, **only the XPath expressions need updating** — the rest of the script stays the same. Pull requests for XPath fixes or any other improvements are very welcome.

> **Note:** You don't need to manually bump the script version when making changes — it updates itself automatically via the `GITHUB_RUN_NUMBER` mechanism.

---

