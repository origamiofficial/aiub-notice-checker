import requests
from lxml import html
import sqlite3
import os

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

# SQLite database information
DB_NAME = "aiub_notices.db"
DB_TABLE_NAME = "notices"

# Script version
SCRIPT_VERSION = "1.7"
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
                online_version = line.split("=")[1].strip()
                break
        # Compare versions and update if necessary
        if online_version > SCRIPT_VERSION:
            print(f"New version {online_version} available. Updating script...")
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

# Visit AIUB Notice page and check for new posts
print("Checking for new posts on AIUB Notice page...")
try:
    page = requests.get(WEBSITE_URL)
    tree = html.fromstring(page.content)
    posts = tree.xpath(POST_XPATH)
    print(f"{len(posts)} posts found on AIUB Notice page.")
except Exception as e:
    print(f"Error checking for new posts on AIUB Notice page: {e}. Check if XPath expressions need to be updated. Exiting script.")
    exit()

# Check if database file exists
if os.path.exists(DB_NAME):
    print(f"Existing SQLite database file found.")
else:
    print(f"Existing SQLite database file not found, created one")

# Connect to SQLite database
print("Connecting to SQLite database...")
try:
    conn = sqlite3.connect(DB_NAME)
    c = conn.cursor()
    print("Connection to database successful.")
except Exception as e:
    print(f"Error connecting to database: {e}. Exiting script.")
    exit()

# Check if notices table exists in database, and create it if it doesn't
try:
    c.execute(
        "CREATE TABLE IF NOT EXISTS {} (title text, description text, link text)".format(
            DB_TABLE_NAME
        )
    )
except Exception as e:
    print(f"Error creating notices table in database: {e}. Exiting script.")
    exit()

# Check if table was created or already exists
try:
    c.execute("SELECT * FROM sqlite_master WHERE type='table' AND name=?", (DB_TABLE_NAME,))
    if c.fetchone():
        print("Notices table already exists in database.")
    else:
        print("Notices table created in database.")
except Exception as e:
    print(f"Error checking if notices table exists: {e}. Exiting script.")
    exit()

# Define the send_telegram_message function
def send_telegram_message(title, description, link, day, month, year):
    # Use the telegram information provided in the script to construct the URL for the API
    telegram_api_url = f"https://api.telegram.org/bot{TELEGRAM_BOT_API_KEY}/sendMessage"

    # Use the URL and XPath information provided in the script to extract the title, description, and link
    # for each post on the AIUB Notice page
    message = f"{title}\nDate: {day} {month} {year}\n\n{description}\n\nhttps://www.aiub.edu{link}"

    # Use the requests module to send a POST request to the telegram API URL with the necessary
    # parameters to send a message to the specified chat
    response = requests.post(
        telegram_api_url,
        data={"chat_id": TELEGRAM_CHAT_ID, "text": message, "disable_web_page_preview": True},
    )

    # Check if the request was successful, and print the response from the server
    if response.status_code == 200:
        print(f"Successfully sent message to Telegram.")
    else:
        print(
            f"Error sending message to Telegram: {response.text}. Exiting script."
        )
        exit()

# Iterate through each post on the AIUB Notice page and check if it exists in the database
for post in posts:
    # Use the XPath expressions provided in the script to extract the title, description, link, day, month, and year
    # for each post
    title = post.xpath(TITLE_XPATH)[0]
    description = post.xpath(DESCRIPTION_XPATH)[0]
    link = post.xpath(LINK_XPATH)[0]
    day = post.xpath(DAY_XPATH)[0]
    month = post.xpath(MONTH_XPATH)[0]
    year = post.xpath(YEAR_XPATH)[0]

    # Check if post is already in database
    c.execute(
        "SELECT * FROM {} WHERE title=? AND description=? AND link=?".format(
            DB_TABLE_NAME
        ),
        (title, description, link),
    )
    # Check if the query returned any rows
    if c.fetchone() is None:
        # Send post to Telegram
        send_telegram_message(title, description, link, day, month, year)
        print(f"Sent post to Telegram: {title}")
        # Insert post into database
        c.execute(
            "INSERT INTO {} (title, description, link) VALUES (?, ?, ?)".format(
                DB_TABLE_NAME
            ),
            (title, description, link),
        )

# Save changes to database and close connection
conn.commit()
conn.close