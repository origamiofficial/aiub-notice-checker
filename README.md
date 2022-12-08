# AIUB Notice Checker
[![Notice Checker](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/notice-checker.yaml/badge.svg)](https://github.com/origamiofficial/aiub-notice-checker/actions/workflows/notice-checker.yaml)

A Python script that checks for new or edited posts on the [AIUB Notice page](https://aiub.cf/category/notices/) and sends updates to a specified Telegram channel.

## Features

- Automatically checks for updates to the script and updates itself if necessary.
- Checks if the AIUB website is up before attempting to access the notice page.
- Saves information about old posts in a SQLite database to check for edits.
- Sends formatted updates to a specified Telegram channel using the Telegram Bot API.

## Requirements

- Python 3.6 or higher
- [lxml](https://lxml.de/) library
- [requests](https://requests.readthedocs.io/en/latest/) library
- [SQLite](https://www.sqlite.org/index.html) database
- Telegram bot with API key and channel username

## Usage

1. Install the required libraries by running the following command:

```bash
pip install lxml requests
```


2. Create a new SQLite database and a `notices` table with the following columns: `title`, `description`, and `link`.

3. Edit the script to include your Telegram bot information, AIUB Notice page URL and XPath information, SQLite database information, and desired script version.

4. Run the script using the following command:

```bash
python main.py
```


The script will automatically check for updates and update itself if necessary. It will then check if the AIUB website is up and visit the notice page to check for new or edited posts. If it finds any, it will send updates to the specified Telegram channel.

## Maintenance

To update the script to a newer version, simply edit the `SCRIPT_VERSION` variable in the script and run it as usual. The script will check for updates and update itself if the new version is available.

## How it works

The script uses the [lxml](https://lxml.de/) library to parse the HTML of the AIUB Notice page and extract the information for each post using the specified XPath values. It then connects to the SQLite database and checks if the notice is already in the `notices` table. If it is not, the script adds the notice to the table and sends an update to the Telegram channel using the [Telegram Bot API](https://core.telegram.org/bots/api). If the notice is already in the table, the script compares the values in the database with the current values for the notice and sends an update if there are any differences.

## Credit

Everything in this repo was developed with the help of OpenAI's GPT-3 language model. Thank you, OpenAI!

[![Hits](https://hits.seeyoufarm.com/api/count/incr/badge.svg?url=https://github.com/origamiofficial/aiub-notice-checker&icon=github.svg&icon_color=%23FFFFFF&title=hits&edge_flat=false)](https://github.com/origamiofficial/aiub-notice-checker)