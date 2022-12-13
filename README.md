# AIUB Notice Checker
[![Facebook](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Facebook.svg)](https://facebook.com/aiubnotice) [![Telegram](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Telegram.svg)](https://t.me/aiubnotice) [![Twitter](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Twitter.svg)](https://twitter.com/aiubnotice) [![LinkedIn](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/LinkedIN.svg)](https://linkedin.com/in/aiubnotice) [![Discord](https://raw.githubusercontent.com/gauravghongde/social-icons/master/SVG/Color/Discord.svg)](https://discord.gg/M8XVrA2Fnb) <br /> [![AIUB Notice Checker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/aiub-notice-checker.yml)

A Python script that checks for new or edited posts on the [AIUB Notice page](https://aiub.cf/category/notices/) and sends updates to a specified Telegram channel.

## Features

- Automatically checks for updates to the script and updates itself if necessary.
- Checks if the AIUB website is up before attempting to access the notice page.
- Saves information about old posts in a SQLite database to check for edits.
- Sends formatted updates to a specified Telegram channel using the Telegram Bot API.

## Requirements

- Python 3.6 or higher
- `requests` library
- `lxml` library
- `sqlite3` library
- `TELEGRAM_CHAT_ID` and `TELEGRAM_BOT_API_KEY` environment variables with valid values

## Usage

1. Clone or download this repository
2. Install the required libraries by:
```bash
pip install -r requirements.txt
```
3. Set the `TELEGRAM_CHAT_ID` and `TELEGRAM_BOT_API_KEY` environment variables with your Telegram chat ID and bot API key
4. Run the script using 
```bash
python main.py
```

## Contribution

If the administrators make any changes and break things, we will only need to update the XPath. I would be incredibly grateful for any pull requests that you might have. Just remember, there is no need to update the script version if you have made changes – it will be updated automatically.

## How it works

The AIUB Notice Checker script is written in Python and uses several libraries to perform its tasks. It uses the `requests` library to fetch the AIUB Notice page, and it uses the `lxml` library to parse the HTML on the page and extract the relevant information about the posts using the specified XPath values. It then connects to the SQLite database and checks if the notice is already in the `notices` table. If it is not, the script adds the notice to the table and sends an update to the Telegram channel using the [Telegram Bot API](https://core.telegram.org/bots/api). The script uses the TELEGRAM_CHAT_ID and TELEGRAM_BOT_API_KEY environment variables to send notifications to a Telegram chat. These values must be set before running the script. If the notice is already in the table, the script compares the values in the database with the current values for the notice and sends an update if there are any differences. The script also checks for updates to itself and updates if a newer version is available. This is done by fetching the latest version of the script from a remote location and comparing it to the current version. If the remote version is newer, the script is updated and restarted automatically.

## Credit

Everything in this repo developed using natural language processing capabilities from OpenAI's GPT-3.

[![Hits](https://hits.seeyoufarm.com/api/count/incr/badge.svg?url=https://github.com/origamiofficial/aiub-notice-checker&icon=github.svg&icon_color=%23FFFFFF&title=hits&edge_flat=false)](https://github.com/origamiofficial/aiub-notice-checker)
