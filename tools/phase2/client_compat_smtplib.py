"""SMTP client compatibility check with Python's smtplib (Phase 2 acceptance).

Usage: python -I client_compat_smtplib.py <path-to-spoolbox>
Starts its own server instances on free ports and checks the replies smtplib sees.
"""
import json, os, smtplib, socket, ssl, subprocess, sys, time
from email.message import EmailMessage

BIN = sys.argv[1]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start(*extra):
    smtp, http = free_port(), free_port()
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MP_", "SPOOLBOX_"))}
    p = subprocess.Popen([BIN, "--smtp", f"127.0.0.1:{smtp}", "--listen", f"127.0.0.1:{http}", "--min-free-disk", "0", "-q", *extra],
                         env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(200):
        try:
            socket.create_connection(("127.0.0.1", smtp), timeout=0.05).close()
            return p, smtp
        except OSError:
            time.sleep(0.02)
    p.kill()
    sys.exit("server did not start")


def message(subject, to="rcpt@example.com"):
    m = EmailMessage()
    m["From"] = "Sender <sender@example.com>"
    m["To"] = to
    m["Subject"] = subject
    m.set_content("body\n.leading dot\n")
    return m


results = []


def check(name, fn):
    try:
        fn()
        results.append((name, "ok", ""))
    except Exception as e:  # noqa: BLE001 - report every failure
        results.append((name, "FAIL", f"{type(e).__name__}: {e}"))


def plain_and_multiple():
    p, port = start()
    try:
        with smtplib.SMTP("127.0.0.1", port, timeout=10) as s:
            code, _ = s.ehlo()
            assert code == 250
            for ext in ("size", "pipelining", "8bitmime", "smtputf8", "enhancedstatuscodes"):
                assert s.has_extn(ext), ext
            for i in range(5):
                refused = s.send_message(message(f"multi {i}"))
                assert refused == {}, refused
    finally:
        p.kill()


def starttls_auth(mech):
    def run():
        p, port = start("--smtp-tls-cert", "auto", "--smtp-require-starttls", "--smtp-auth-accept-any")
        try:
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            with smtplib.SMTP("127.0.0.1", port, timeout=10) as s:
                s.ehlo()
                assert s.has_extn("starttls")
                try:
                    s.sendmail("a@example.com", ["b@example.com"], b"Subject: x\r\n\r\nx")
                    raise AssertionError("expected 530 before STARTTLS")
                except smtplib.SMTPSenderRefused as e:
                    assert e.smtp_code == 530, e
                s.starttls(context=ctx)
                s.ehlo()
                assert not s.has_extn("starttls")
                s.user, s.password = "user", "secret"
                if mech == "PLAIN":
                    code, _ = s.auth("PLAIN", s.auth_plain)
                else:
                    code, _ = s.auth("LOGIN", s.auth_login, initial_response_ok=False)
                assert code == 235, code
                assert s.send_message(message(f"tls {mech}")) == {}
        finally:
            p.kill()
    return run


def smtputf8_address():
    p, port = start()
    try:
        with smtplib.SMTP("127.0.0.1", port, timeout=10) as s:
            s.ehlo()
            m = message("utf8", to="کاربر@مثال.ایران")
            assert s.send_message(m, mail_options=["SMTPUTF8"]) == {}
    finally:
        p.kill()


def attachment_and_size_limit():
    p, port = start("--max-message-size", "1")
    try:
        with smtplib.SMTP("127.0.0.1", port, timeout=10) as s:
            m = message("attachment")
            m.add_attachment(b"%PDF-1.4 small", maintype="application", subtype="pdf", filename="r.pdf")
            assert s.send_message(m) == {}
            big = message("too big")
            big.set_content("x" * (2 * 1024 * 1024))
            try:
                s.send_message(big)
                raise AssertionError("expected 552")
            except (smtplib.SMTPSenderRefused, smtplib.SMTPDataError) as e:
                assert e.smtp_code == 552, e
            s.noop()
    finally:
        p.kill()


def implicit_tls():
    p, port = start("--smtp-tls-cert", "auto", "--smtp-require-tls")
    try:
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        with smtplib.SMTP_SSL("127.0.0.1", port, context=ctx, timeout=10) as s:
            assert s.send_message(message("smtps")) == {}
    finally:
        p.kill()


check("plain EHLO extensions + 5 messages on one connection", plain_and_multiple)
check("STARTTLS required + AUTH PLAIN", starttls_auth("PLAIN"))
check("STARTTLS required + AUTH LOGIN", starttls_auth("LOGIN"))
check("SMTPUTF8 recipient (Persian IDN)", smtputf8_address)
check("attachment + 552 size limit, session continues", attachment_and_size_limit)
check("implicit TLS (SMTP_SSL)", implicit_tls)

print(json.dumps({"python": sys.version.split()[0], "results": results}, ensure_ascii=False, indent=2))
sys.exit(0 if all(r[1] == "ok" for r in results) else 1)
