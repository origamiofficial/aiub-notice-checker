import requests
from lxml import html
import sqlite3
import time

# Telegram bot information
TELEGRAM_CHANNEL_USERNAME = "AIUB_Notice_Updates"
TELEGRAM_BOT_API_KEY = "1234567890:ABCDEFGHIJKLMNOPQRSTUVWXYZ"

# URL and XPath information for AIUB Notice page
WEBSITE_URL = "https://aiub.cf/category/notices/"
POST_XPATH = "/html/body/div/div/div/ul/li/div"
TITLE_XPATH = "h2[1]/child::node()"
LINK_XPATH = "a[1]/ancestor-or-self::node()/@href"
DESCRIPTION_XPATH = "p[1]/child::node()"

# SQLite database information
DB_NAME = "aiub_notices.db"
DB_TABLE_NAME = "notices"

# Script version
SCRIPT_VERSION = "1.2"
SCRIPT_URL = "https://raw.githubusercontent.com/origamiofficial/aiub-notice-checker/main/main.py"

# Check for script updates
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
        # Download new version of script
        with open("main.py", "w") as f:
            f.write(response.text)
        # Run new version of script and exit current script
        os.execv(sys.executable, ["python"] + sys.argv)
        sys.exit()

# Check if AIUB website is up
try:
    requests.get(WEBSITE_URL)
except requests.ConnectionError:
    print("AIUB website is down. Exiting script.")
    exit()

# Visit AIUB Notice page and check for new posts
page = requests.get(WEBSITE_URL)
tree = html.fromstring(page.content)
posts = tree.xpath(POST_XPATH)

# Connect to SQLite database
conn = sqlite3.connect(DB_NAME)
c = conn.cursor()

# Check if notices table exists in database, and create it if it doesn't
c.execute(
    "CREATE TABLE IF NOT EXISTS {} (title text, description text, link text)".format(
        DB_TABLE_NAME
    )
)

# Check each post and send update if it's new or edited
for post in posts:
    title = post.xpath(TITLE_XPATH)[0].strip()
    description = post.xpath(DESCRIPTION_XPATH)[0].strip()
    link = post.xpath(LINK_XPATH)[0].strip().replace("aiub.cf", "www.aiub.edu")

    # Check if notice is already in database
    c.execute("SELECT * FROM {} WHERE title = ?".format(DB_TABLE_NAME), (title,))
    result = c.fetchone()

    # If notice is not in database, add it and send update
    if result is None:
        c.execute(
            "INSERT INTO {} (title, description, link) VALUES (?, ?, ?)".format(
                DB_TABLE_NAME
            ),
            (title, description, link),
        )
        send_telegram_message(title, description, link)

    # If notice is in database, check if it has been edited and send update if necessary
    else:
        if (
            description != result[1]
            or link != result[2]
        ):
            # Update notice in database
            c.execute(
                "UPDATE {} SET description = ?, link = ? WHERE title = ?".format(
                    DB_TABLE_NAME
                ),
                (description, link, title),
            )
            send_telegram_message(title, description, link)

# Close database connection
conn.close()

# Function to send message to Telegram channel
def send_telegram_message(title, description, link):
    message = "[{}]\n\n{}\n\n{}".format(title, description, link)
    requests.get(
        "https://api.telegram.org/bot{}/sendMessage?chat_id={}&text={}".format(
            TELEGRAM_BOT_API_KEY, TELEGRAM_CHANNEL_USERNAME, message
        )
    )
