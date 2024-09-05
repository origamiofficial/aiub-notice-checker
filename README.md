# AIUB Notice Checker
[![Facebook](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Facebook.svg)](https://facebook.com/aiubnotice) [![Telegram](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Telegram.svg)](https://t.me/aiubnotice) [![Twitter](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Twitter.svg)](https://twitter.com/aiubnotice) [![LinkedIn](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/LinkedIN.svg)](https://linkedin.com/in/aiubnotice) [![Discord](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Discord.svg)](https://discord.gg/M8XVrA2Fnb) <br />
[![AIUB Notice Checker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml) ![We Support](https://img.shields.io/badge/we%20stand%20with-%F0%9F%87%B5%F0%9F%87%B8%20palestine-white.svg)

### A Python script that checks for new or edited posts on the [AIUB Notice page](https://www.aiub.edu/category/notices/) and sends updates to a specified Telegram channel.

## RSS Feed | [![Valid RSS](https://validator.w3.org/feed/images/valid-rss-rogers.png)](http://validator.w3.org/feed/check.cgi?url=https%3A//github.com/origamiofficial/aiub-notice-checker/raw/main/rss.xml) 
```
https://github.com/origamiofficial/aiub-notice-checker/raw/main/rss.xml
```

## Features

- Automatically checks for updates to the script and updates itself if necessary.
- Checks if the AIUB website is up before attempting to access the notice page.
- Checks if the XPaths values are working or needs to be updated.
- Saves information about old posts in a SQLite database to check for edits.
- Sends formatted updates to a specified Telegram channel using the Telegram Bot API.
- Generates RSS feed from the SQLite database using RSS 2.0 format.

## Requirements

- Python 3.6 or higher
- `requests` library
- `lxml` library
- `TELEGRAM_CHAT_ID`, `TELEGRAM_ADMIN_CHAT_ID`, `TELEGRAM_BOT_API_KEY` and `GITHUB_RUN_NUMBER` environment variables with valid values

## Usage

1. Clone or download this repository by:
```bash
git clone origamiofficial/aiub-notice-checker
```
2. Install the required libraries by:
```bash
pip install -r requirements.txt
```
3. Set the environment variables outside the script according to your Telegram info
4. Run the script using 
```bash
python main.py
```

## Contribution

If the administrators make any changes and break things, we will only need to update the XPath. I would be incredibly grateful for any pull requests that you might have. Just remember, there is no need to update the script version if you have made changes – it will be updated automatically.

## How it works

The Python script automates the process of checking for new or edited notices on the AIUB Notice page and sending updates to a Telegram channel. It first retrieves environment variables and checks for script updates. Then, it verifies the accessibility of the AIUB website and validates XPath expressions for extracting data from the webpage. Next, it connects to a local SQLite database or creates a new one if it doesn't exist. By iterating through each notice on the AIUB Notice page, the script compares it with the database entries and updates the database accordingly. It also generates an RSS feed containing the latest notices. Finally, it sends Telegram messages for new or edited notices and closes the database connection.

## Credit

Everything in this repo was developed using natural language processing capabilities from OpenAI's GPT-3.

[![Hits](https://hits.seeyoufarm.com/api/count/incr/badge.svg?url=https://github.com/origamiofficial/aiub-notice-checker&icon=github.svg&icon_color=%23FFFFFF&title=hits&edge_flat=false)](https://github.com/origamiofficial/aiub-notice-checker)
