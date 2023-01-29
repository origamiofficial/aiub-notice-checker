import requests
from lxml import html
import sqlite3
import os
import sys

# Telegram information
TELEGRAM_CHAT_ID = os.environ["TELEGRAM_CHAT_ID"]
TELEGRAM_BOT_API_KEY = os.environ["TELEGRAM_BOT_API_KEY"]

# URL and XPath information for AIUB Notice page
WEBSITE_URL = "https://aiub.cf/category/notices/"
POST_XPATH = "//ul[@class='event-list']/li"
TITLE_XPATH = ".//h2[@class='title']/text()"
LINK_XPATH = ".//a[@class='info-link']/@href"
DESCRIPTION_XPATH = ".//p[@class='desc']/text()"
DAY_XPATH = ".//time/span[@class='day']/text()"
MONTH_XPATH = ".//time/span[@class='month']/text()"
YEAR_XPATH = ".//time/span[@class='year']/text()"

# Message format for new notices
NEW_NOTICE_MESSAGE_FORMAT = (
    "{title}\n\n"
    "Date: {day} {month} {year}\n\n"
    "{description}\n\n"
    "https://www.aiub.edu{link}"
)

# Message format for edited notices
EDITED_NOTICE_MESSAGE_FORMAT = (
    "[Edited] {title}\n\n"
    "Date: {day} {month} {year}\n\n"
    "{description}\n\n"
    "https://www.aiub.edu{link}"
)

# SQLite database information
DB_NAME = "aiub_notices.db"
DB_TABLE_NAME = "notices"

# Script version
SCRIPT_VERSION = "2.3"
SCRIPT_URL = "https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/main.py"

# Check for script updates
print("Checking for script updates...")
try:
    response = requests.get(SCRIPT_URL)
    if response.status_code == 200:
        # Parse version information from script
        lines = response.text.split("\n")
        for line in lines:
            if line.startswith("SCRIPT_VERSION"):
                ONLINE_VERSION = line.split("=")[1].strip().strip('"')
                break
        # Print current and online versions
        print(f"Current version: {SCRIPT_VERSION}")
        print(f"Online version: {ONLINE_VERSION}")
        # Compare versions and update if necessary
        if ONLINE_VERSION > SCRIPT_VERSION:
            print(f"New version {ONLINE_VERSION} available. Updating script...")
            # Download new version of script
            with open("main.py", "w") as f:
                f.write(response.text)
            # Run new version of script and exit current script
            os.execv(sys.executable, ["python"] + sys.argv)
            sys.exit()
        else:
            print("Script is up to date.")
except Exception as e:
    print(f"Error checking for script updates: {e}")

# Check if AIUB website is up
print("Checking if AIUB website is up...")
try:
    requests.get(WEBSITE_URL)
    print("AIUB website is up.")
except requests.ConnectionError as e:
    print(f"AIUB website is down: {e}. Exiting script.")
    exit()

def check_xpath(tree, xpaths):
    invalid_xpaths = []
    for xpath_name, xpath in xpaths.items():
        element_name = xpath.split("/")[-1]
        elements = tree.xpath(xpath)
        if len(elements) == 0:
            invalid_xpaths.append((xpath_name, element_name))
    return invalid_xpaths

try:
    page = requests.get(WEBSITE_URL)
    tree = html.fromstring(page.content)
    xpaths = {
        "POST_XPATH": POST_XPATH,
        "TITLE_XPATH": TITLE_XPATH,
        "LINK_XPATH": LINK_XPATH,
        "DESCRIPTION_XPATH": DESCRIPTION_XPATH,
        "DAY_XPATH": DAY_XPATH,
        "MONTH_XPATH": MONTH_XPATH,
        "YEAR_XPATH": YEAR_XPATH
    }
    invalid_xpaths = check_xpath(tree, xpaths)
    if invalid_xpaths:
        print(f"Error: Invalid XPath expressions found:")
        for xpath_name, element_name in invalid_xpaths:
            print(f"{xpath_name}: No {element_name} found")
        print("XPath expressions may need to be updated. Exiting script.")
        exit()
except Exception as e:
    print(f"Error checking XPath expressions: {e}. XPath expressions may need to be updated. Exiting script.")
    exit()
print("All XPath expressions are valid.")

# Visit AIUB Notice page and check for new posts
print("Checking for new posts on AIUB Notice page...")
try:
    page = requests.get(WEBSITE_URL)
    tree = html.fromstring(page.content)
    posts = tree.xpath(POST_XPATH)
    if len(posts) == 0:
        print("NO POSTS WERE FOUND on the AIUB Notice page. Check if XPath expressions need to be updated. Exiting script.")
        exit()
    else:
        print(f"{len(posts)} posts found on AIUB Notice page.")
except Exception as e:
    print(f"Error checking for new posts on AIUB Notice page: {e}. Check if XPath expressions need to be updated. Exiting script.")
    exit()

# Check if database file exists
print(f"Checking if database file exists...")
if os.path.exists(DB_NAME):
    print(f"Existing SQLite database file found.")
    # Open connection to database
    conn = sqlite3.connect(DB_NAME)
    c = conn.cursor()
    
    # Count number of notices in database
    c.execute(f"SELECT COUNT(*) FROM {DB_TABLE_NAME}")
    notice_count = c.fetchone()[0]
    print(f"{notice_count} notices found in database.")
    
    # Check for moved notices in database
    print(f"Checking for moved notices in website...")
    c.execute(f"SELECT link, title FROM {DB_TABLE_NAME}")
    database_links = c.fetchall()
    for db_link, title in database_links:
        # Check if link is still on AIUB Notice page
        found = False
        for post in posts:
            web_link = "".join(post.xpath(LINK_XPATH)).strip()
            if db_link == web_link:
                found = True
                break
        if not found:
            # Link not found on AIUB Notice page, delete from database
            c.execute(f"DELETE FROM {DB_TABLE_NAME} WHERE link=?", (db_link,))
            conn.commit()
            print(f"Deleting Notice: '{title}' from database.")

    # Close connection to database
    conn.commit()
    conn.close()
else:
    print(f"Existing SQLite database file NOT found. Creating new...")
    conn = sqlite3.connect(DB_NAME)
    c = conn.cursor()
    c.execute(
        f"CREATE TABLE {DB_TABLE_NAME} ("
        "id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "title TEXT NOT NULL,"
        "description TEXT NOT NULL,"
        "link TEXT NOT NULL,"
        "day INTEGER NOT NULL,"
        "month INTEGER NOT NULL,"
        "year INTEGER NOT NULL"
        ")"
    )
    conn.commit()
    print(f"New SQLite database file created.")

# Connect to database
conn = sqlite3.connect(DB_NAME)
c = conn.cursor()

# Send message to Telegram chat
def send_telegram_message(message):
    # Send message to Telegram chat
    url = f"https://api.telegram.org/bot{TELEGRAM_BOT_API_KEY}/sendMessage"
    payload = {
        "chat_id": TELEGRAM_CHAT_ID,
        "text": message,
		"disable_web_page_preview": "true"
    }
    try:
        response = requests.post(url, json=payload)
        print(f"Sending Notice: '{title}'.")
        # Check if the request was successful, and print the response from the server
        if response.status_code == 200:
            print(f"Successfully sent message to Telegram.")
        else:
            print(
                f"Error sending message to Telegram: {response.text}. Exiting script."
            )
            exit()
    except Exception as e:
        print(f"Error sending message to Telegram: {e}")

# Iterate through posts and check for new or edited notices
for post in posts:
    # Retrieve data for post
    title = "".join(post.xpath(TITLE_XPATH)).strip()
    link = "".join(post.xpath(LINK_XPATH)).strip()
    description = "".join(post.xpath(DESCRIPTION_XPATH)).strip()
    day = "".join(post.xpath(DAY_XPATH)).strip()
    month = "".join(post.xpath(MONTH_XPATH)).strip()
    year = "".join(post.xpath(YEAR_XPATH)).strip()
    # Check if notice has been seen before
    c.execute(f"SELECT * FROM {DB_TABLE_NAME} WHERE link=?", (link,))
    result = c.fetchone()
    if result is None:
        # Notice is new, add to database and send message
        c.execute(
            f"INSERT INTO {DB_TABLE_NAME} "
            "(title, description, link, day, month, year) VALUES (?,?,?,?,?,?)",
            (title, description, link, day, month, year)
        )
        conn.commit()
        message = NEW_NOTICE_MESSAGE_FORMAT.format(
            title=title,
            day=day,
            month=month,
            year=year,
            description=description,
            link=link
        )
        send_telegram_message(message)
    else:
        # Notice has been seen before, check if it has been edited
        old_title = result[1]
        old_description = result[2]
        if old_title != title or old_description != description:
            # Notice has been edited, update database and send message
            c.execute(
                f"UPDATE {DB_TABLE_NAME} "
                "SET title=?, description=? "
                "WHERE link=?",
                (title, description, link)
            )
            conn.commit()
            message = EDITED_NOTICE_MESSAGE_FORMAT.format(
                title=title,
                day=day,
                month=month,
                year=year,
                description=description,
                link=link
            )
            send_telegram_message(message)

# Close database connection
conn.commit()
conn.close()

print("Script completed.")