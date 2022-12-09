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
TITLE_XPATH = "/html/body/div/div/div/ul/li/div/h2/text()"
LINK_XPATH = "/html/body/div/div/div/ul/li/a/@href"
DESCRIPTION_XPATH = "/html/body/div/div/div/ul/li/div/p/text()"

# SQLite database information
DB_NAME = "aiub_notices.db"
DB_TABLE_NAME = "notices"

# Script version
SCRIPT_VERSION = "1.3"
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

# Define the send_telegram_message function
def send_telegram_message(title, description, link):
    # Use the telegram bot information provided in the script to construct the URL for the API
    telegram_api_url = f"https://api.telegram.org/bot{TELEGRAM_BOT_API_KEY}/sendMessage"

    # Construct the message to send
    message = f"{title}\n\n{description}\n\n{link}"

    # Send the message to the telegram channel
    requests.post(telegram_api_url, data={"chat_id": TELEGRAM_CHANNEL_USERNAME, "text": message})


# Check each post and send update if it's new or edited
for post in posts:
    title = post.xpath(TITLE_XPATH)[0].strip()
    description = post.xpath(DESCRIPTION_XPATH)[0].strip()
    link = post.xpath(LINK_XPATH)[0].strip().replace("aiub.cf", "www.aiub.edu")

    # Check if the notice already exists in the database
    c.execute("SELECT * FROM {} WHERE title=? AND description=? AND link=?".format(DB_TABLE_NAME), (title, description, link))
    notice = c.fetchone()

    # If the notice doesn't exist in the database, insert it
    if notice is None:
        c.execute("INSERT INTO {} VALUES (?, ?, ?)".format(DB_TABLE_NAME), (title, description, link))

    # If the notice exists in the database but has been edited, update the database and send a notification
    elif notice[0] == title and notice[1] == description and notice[2] == link:
        c.execute("UPDATE {} SET title=?, description=?, link=? WHERE title=? AND description=? AND link=?".format(DB_TABLE_NAME), (title, description, link, title, description, link))
        send_telegram_message(title, description, link)

# Save changes to the database and close the connection
conn.commit()
conn.close()
