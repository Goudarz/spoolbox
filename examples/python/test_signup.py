"""Testing a sign-up email with Spoolbox, using only the Python standard library.

Run a server (`spoolbox`), then: python examples/python/test_signup.py
Set SPOOLBOX_URL / SPOOLBOX_SMTP if it does not listen on the defaults.
"""
import json
import os
import smtplib
import unittest
import urllib.request
from email.message import EmailMessage

API = os.environ.get("SPOOLBOX_URL", "http://127.0.0.1:8025/").rstrip("/") + "/"
SMTP_HOST, SMTP_PORT = os.environ.get("SPOOLBOX_SMTP", "127.0.0.1:1025").rsplit(":", 1)


def api(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(API + path, data=data, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as res:
        return json.load(res)


def send_signup_email(address):
    """Stands in for the application under test."""
    msg = EmailMessage()
    msg["From"] = "Acme <noreply@acme.test>"
    msg["To"] = address
    msg["Subject"] = "Confirm your email"
    msg.set_content("Your verification code is 482913.\nhttps://acme.test/verify?token=abc123\n")
    with smtplib.SMTP(SMTP_HOST, int(SMTP_PORT)) as s:
        s.send_message(msg)


class SignupEmail(unittest.TestCase):
    def test_code_arrives(self):
        # Take the cursor first, so an email that arrives quickly is not missed.
        cursor = api("api/v2/events/cursor")["seq"]
        send_signup_email("alice+python@example.com")
        msg = api("api/v2/wait", {"query": "to:alice+python@example.com", "since_seq": cursor, "timeout_ms": 10000})
        self.assertEqual(msg["subject"], "Confirm your email")
        codes = api(f"api/v2/messages/{msg['id']}/codes")
        self.assertEqual(codes[0]["value"], "482913")
        links = api(f"api/v2/messages/{msg['id']}/links")
        self.assertIn("https://acme.test/verify?token=abc123", [l["url"] for l in links])


if __name__ == "__main__":
    unittest.main()
