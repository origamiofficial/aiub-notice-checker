import requests
from lxml import html
import sqlite3
import os
import sys
import xml.etree.ElementTree as ET
import datetime

# Environment variable information
TELEGRAM_CHAT_ID = os.environ["TELEGRAM_CHAT_ID"]
TELEGRAM_ADMIN_CHAT_ID = os.environ.get("TELEGRAM_ADMIN_CHAT_ID")
TELEGRAM_BOT_API_KEY = os.environ["TELEGRAM_BOT_API_KEY"]
GITHUB_RUN_NUMBER = os.environ["GITHUB_RUN_NUMBER"]
PROTOCOLS = ["https://", "http://"]
NOTICE_PAGE = "www.aiub.edu/category/notices"
WEBSITE_URL = None # DO NOT CHANGE

# XPath information for AIUB Notice page
POST_XPATH = "//div[contains(@class, 'notification')]"
TITLE_XPATH = ".//h2[@class='title']/text()"
LINK_XPATH = ".//a[@class='info-link']/@href"
DESCRIPTION_XPATH = ".//p[@class='desc']/text()"
DAY_XPATH = ".//div[contains(@class, 'date-custom')]/normalize-space(text()[1])"
MONTH_XPATH = ".//div[contains(@class, 'date-custom')]/normalize-space(text()[2])"
YEAR_XPATH = ".//div[contains(@class, 'date-custom')]/span/normalize-space(text())"

# Message format for new notices
NEW_NOTICE_MESSAGE_FORMAT = (
    "{title}\n\n"
    "Date: {day} {month} {year}\n\n"
    "{description}\n\n"
    "https://www.aiub.edu{link}#{gh_run_no}"
)

# Message format for edited notices
EDITED_NOTICE_MESSAGE_FORMAT = (
    "[EDITED] {title}\n\n"
    "Date: {day} {month} {year}\n\n"
    "{description}\n\n"
    "https://www.aiub.edu{link}#{gh_run_no}"
)

# SQLite database information
DB_NAME = "aiub_notices.db"
DB_TABLE_NAME = "notices"

# RSS feed information
RSS_FEED_FILE = "rss.xml"
DEFAULT_TIME = "00:00:00"

# Script version
SCRIPT_VERSION = "3.5"
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
for protocol in PROTOCOLS:
    try:
        response = requests.get(protocol + NOTICE_PAGE)
        if response.status_code == 200:
            WEBSITE_URL = protocol + NOTICE_PAGE
            print(f"AIUB website is up. Protocol: {protocol} is working.")
            break
    except requests.exceptions.RequestException as e:
        print(f"Error trying {protocol}: {e}")

if WEBSITE_URL is None:
    print("AIUB website is down. Exiting script.")
    exit()

# Function to send admin notification
def send_admin_notification(message):
    admin_url = f"https://api.telegram.org/bot{TELEGRAM_BOT_API_KEY}/sendMessage"
    admin_payload = {
        "chat_id": TELEGRAM_ADMIN_CHAT_ID,
        "text": message,
        "disable_web_page_preview": "true"
    }
    try:
        admin_response = requests.post(admin_url, json=admin_payload)
        if admin_response.status_code != 200:
            print(f"Error sending admin notification: {admin_response.text}")
    except Exception as e:
        print(f"Error sending admin notification: {e}")

def check_xpath(tree, xpaths):
    invalid_xpaths = []
    for xpath_name, xpath in xpaths.items():
        element_name = xpath.split("/")[-1]
        elements = tree.xpath(xpath)
        if len(elements) == 0:
            invalid_xpaths.append((xpath_name, element_name))
            # Send admin notification for invalid XPath
            admin_notification = f"AIUB Notice\n\nInvalid XPath: {xpath_name} - No {element_name} found.\n\nXPath expressions may need to be updated."
            send_admin_notification(admin_notification)
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
        #if not found:
            # Link not found on AIUB Notice page, delete from database
            #c.execute(f"DELETE FROM {DB_TABLE_NAME} WHERE link=?", (db_link,))
            #conn.commit()
            #print(f"Deleting Notice: '{title}' from database.")

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
            link=link,
            gh_run_no=GITHUB_RUN_NUMBER
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
                link=link,
                gh_run_no=GITHUB_RUN_NUMBER
            )
            send_telegram_message(message)

# Generate RSS feed
def generate_rss_feed():
    c.execute(f"SELECT title, description, link, day, month, year FROM {DB_TABLE_NAME} ORDER BY year DESC, month DESC, day DESC")
    notices = c.fetchall()
    # Root element
    rss = ET.Element("rss", version="2.0")
    channel = ET.SubElement(rss, "channel")
    # Channel elements
    ET.SubElement(channel, "title").text = "AIUB Notices"
    ET.SubElement(channel, "link").text = f"https://{NOTICE_PAGE}"
    ET.SubElement(channel, "description").text = "Latest notices from AIUB."
    # Add notices to RSS feed
    for notice in notices:
        title, description, link, day, month_name, year = notice
        # Extract month as a number (assuming month names are stored as strings)
        month_number = datetime.datetime.strptime(month_name, "%B").month
        # Generate RFC-822 date-time format with default time
        pub_date = datetime.datetime(year=int(year), month=month_number, day=int(day), hour=int(DEFAULT_TIME.split(":")[0]), minute=int(DEFAULT_TIME.split(":")[1]), second=int(DEFAULT_TIME.split(":")[2])).strftime("%a, %d %b %Y %H:%M:%S GMT")
        item = ET.SubElement(channel, "item")
        ET.SubElement(item, "title").text = title
        # Escape special characters
        description = description.replace("&", "&amp;")
        ET.SubElement(item, "description").text = description
        ET.SubElement(item, "link").text = f"https://www.aiub.edu{link}"
        ET.SubElement(item, "pubDate").text = pub_date
        # Add guid element
        guid = ET.SubElement(item, "guid")
        guid.text = f"https://www.aiub.edu{link}"
    # Add atom:link with rel="self"
    self_link = ET.SubElement(channel, "{http://www.w3.org/2005/Atom}link")
    self_link.set("rel", "self")
    self_link.set("type", "application/rss+xml")
    self_link.set("href", "https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/rss.xml")
    # Write to file
    tree = ET.ElementTree(rss)
    tree.write(RSS_FEED_FILE, encoding="UTF-8", xml_declaration=True, method="xml")
    print(f"RSS feed generated at {RSS_FEED_FILE}")

# Generate RSS feed after processing notices
generate_rss_feed()

# Close database connection
conn.commit()
conn.close()

print("Script Completed.")
