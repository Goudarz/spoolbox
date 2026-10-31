# API v1 snapshot test: sends a fixed set of messages to a running Spoolbox, calls every v1
# endpoint and compares the responses with the golden files in fixtures/api-v1/.
#
#   python tools/snapshot/api_v1.py [--api http://127.0.0.1:28125] [--smtp 127.0.0.1:21125] [--record]
#
# The server must start with an empty mailbox. --record rewrites the golden files; review the
# diff before committing it. Exit code 0 when every response matches.
import argparse, base64, hashlib, json, os, re, smtplib, sys, time, urllib.error, urllib.parse, urllib.request
from email.message import EmailMessage
from email.utils import formataddr

ap = argparse.ArgumentParser()
ap.add_argument("--api", default="http://127.0.0.1:28125")
ap.add_argument("--smtp", default="127.0.0.1:21125")
ap.add_argument("--fixtures", default=os.path.join(os.path.dirname(__file__), "..", "..", "fixtures", "api-v1"))
ap.add_argument("--record", action="store_true")
args = ap.parse_args()
API = args.api.rstrip("/")
SMTP = (args.smtp.rsplit(":", 1)[0], int(args.smtp.rsplit(":", 1)[1]))
# Values that change between runs.
VOLATILE = {"Database", "DatabaseSize", "Version", "RuntimeStats"}


def req(method, path, body=None, ctype="application/json"):
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    r = urllib.request.Request(API + path, method=method, data=data)
    if data is not None and ctype:
        r.add_header("Content-Type", ctype)
    try:
        resp = urllib.request.urlopen(r, timeout=30)
        return resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers, e.read()


idmap = {}


def norm(s):
    for real, ph in idmap.items():
        s = s.replace(real, ph)
    s = re.sub(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?(Z|[+-]\d\d:\d\d)", "<TIMESTAMP>", s)
    s = re.sub(r'nonce="[0-9a-f]+"', 'nonce="<NONCE>"', s)
    return s


def strip(v):
    if isinstance(v, dict):
        return {k: ("<VOLATILE>" if k in VOLATILE else strip(x)) for k, x in v.items()}
    if isinstance(v, list):
        return [strip(x) for x in v]
    return v


os.makedirs(args.fixtures, exist_ok=True)
results = []


def cap(name, method, path, body=None, ctype="application/json"):
    st, h, b = req(method, path, body, ctype)
    ct = h.get("Content-Type", "")
    rec = {
        "name": name,
        "request": {"method": method, "path": norm(path),
                    "body": body.decode(errors="replace") if isinstance(body, bytes) else body},
        "status": st,
        "content_type": ct,
        "content_disposition": norm(h.get("Content-Disposition") or "") or None,
    }
    if not (ct.startswith("text") or "json" in ct):
        rec["body_sha256"] = hashlib.sha256(b).hexdigest()
        rec["body_len"] = len(b)
    else:
        t = norm(b.decode("utf-8", "replace"))
        try:
            rec["body_json"] = strip(json.loads(t))
        except Exception:
            rec["body_text"] = t
    if name == "send_api" and isinstance(rec.get("body_json"), dict):
        rec["body_json"]["ID"] = "<ID>" if rec["body_json"].get("ID") else ""
    fn = os.path.join(args.fixtures, f"{name}.json")
    if args.record:
        with open(fn, "w", encoding="utf-8", newline="\n") as f:
            json.dump(rec, f, ensure_ascii=False, indent=2)
            f.write("\n")
        results.append((name, "recorded", []))
    else:
        try:
            with open(fn, encoding="utf-8") as f:
                want = json.load(f)
        except FileNotFoundError:
            results.append((name, "FAIL", ["no golden file (run with --record)"]))
            return st, b
        diffs = []
        for k in ("status", "content_type", "content_disposition", "body_json", "body_text", "body_sha256"):
            if rec.get(k) != want.get(k):
                diffs.append(f"{k}:\n  got:  {json.dumps(rec.get(k), ensure_ascii=False)[:1200]}\n  want: {json.dumps(want.get(k), ensure_ascii=False)[:1200]}")
        results.append((name, "pass" if not diffs else "FAIL", diffs))
    return st, b


PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")


def send(m, frm="envelope@example.com"):
    # Fixed MIME boundaries keep sizes and headers identical between runs.
    raw = m.as_bytes()
    for n, b in enumerate(dict.fromkeys(re.findall(rb"=+\d+==", raw))):
        raw = raw.replace(b, b"==boundary-%d==" % n)
    rcpts = [a.strip().rsplit("<", 1)[-1].rstrip(">") for h in ("To", "Cc") for a in (m.get(h) or "").split(",") if a.strip()]
    with smtplib.SMTP(*SMTP) as s:
        s.sendmail(frm, rcpts, raw)


st, _h, b = req("GET", "/api/v1/messages")
if json.loads(b)["total"] != 0:
    sys.exit("the mailbox must be empty before the snapshot test")

m = EmailMessage()
m["From"] = "Plain Sender <plain@example.com>"; m["To"] = "alice@example.com"
m["Subject"] = "fixture-plain"; m["Message-ID"] = "<plain-1@fixtures.test>"; m["Date"] = "Fri, 23 Oct 2026 10:00:00 +0000"
m.set_content("Hello plain text.\nVisit https://example.invalid/path?a=1\n")
send(m)

m = EmailMessage()
m["From"] = formataddr(("فرستنده آزمایشی", "fa@example.com"))
m["To"] = "bob+signup@example.com, " + formataddr(("کاربر", "user@example.com"))
m["Cc"] = "carol@example.com"; m["Reply-To"] = "reply@example.com"
m["Subject"] = "fixture-html سفارش شما ثبت شد"; m["Message-ID"] = "<html-1@fixtures.test>"
m["Date"] = "Fri, 23 Oct 2026 10:01:00 +0000"
m["X-Tags"] = "Invoice, urgent"
m["List-Unsubscribe"] = "<mailto:unsub@example.com>, <https://example.invalid/unsub>"
m["List-Unsubscribe-Post"] = "List-Unsubscribe=One-Click"; m["X-Custom-Id"] = "abc123"
m.set_content("متن ساده پیام. کد تأیید: 482913")
m.add_alternative('<html><body dir="rtl"><p>سلام <b>دنیا</b></p><img src="cid:logo1">'
                  '<a href="https://example.invalid/verify?code=482913">verify</a>'
                  '<img src="https://example.invalid/pixel.gif" width="1" height="1"></body></html>', subtype="html")
m.get_payload()[1].add_related(PNG, maintype="image", subtype="png", cid="<logo1>", filename="logo.png")
m.add_attachment("سلام".encode(), maintype="text", subtype="plain", filename="گزارش.txt")
send(m)

m = EmailMessage()
m["From"] = "att@example.com"; m["To"] = "dave@example.com"; m["Subject"] = "fixture-attachments"
m["Message-ID"] = "<att-1@fixtures.test>"; m["Date"] = "Fri, 23 Oct 2026 10:02:00 +0000"
m.set_content("see attached")
m.add_attachment(b"<script>alert(1)</script>", maintype="text", subtype="html", filename="evil.html")
m.add_attachment(PNG, maintype="image", subtype="png", filename="pixel.png")
send(m)

raw = b"From: nomsgid@example.com\r\nTo: erin@example.com\r\nSubject: fixture-no-msgid\r\n\r\nbody\r\n"
with smtplib.SMTP(*SMTP) as s:
    s.sendmail("envelope@example.com", ["erin@example.com", "hidden@example.com"], raw)

for _ in range(100):
    st, _h, b = req("GET", "/api/v1/search?query=has%3Aattachment")
    if json.loads(b)["messages_count"] >= 2:
        break
    time.sleep(0.05)

st, _h, b = req("GET", "/api/v1/messages")
by = {mm["Subject"].split()[0]: mm["ID"] for mm in json.loads(b)["messages"]}
for k, v in by.items():
    idmap[v] = f"<ID:{k}>"
H, A, P, N = by["fixture-html"], by["fixture-attachments"], by["fixture-plain"], by["fixture-no-msgid"]

cap("messages_list", "GET", "/api/v1/messages")
cap("messages_list_limit_start", "GET", "/api/v1/messages?start=1&limit=2")
queries = {"word": "fixture", "subject_fa": "subject:سفارش", "from": "from:fa@example.com", "to_plus": "to:bob",
           "tag_xtags": "tag:invoice", "tag_plus": "tag:signup", "unread": "is:unread", "has_att": "has:attachment",
           "not_has_att": "-has:attachment", "larger": "larger:1k", "after_iso": "after:2020-01-01",
           "addressed": "addressed:carol", "phrase": '"verify"', "custom_header_value": "abc123",
           "header_filter": "header:X-Custom-Id=abc123", "message_id": "message-id:html-1", "body_fa": "دنیا"}
for k, q in queries.items():
    cap("search_" + k, "GET", "/api/v1/search?query=" + urllib.parse.quote(q))
cap("search_missing_query", "GET", "/api/v1/search")
cap("message_html", "GET", f"/api/v1/message/{H}")
cap("message_latest", "GET", "/api/v1/message/latest")
cap("message_not_found", "GET", "/api/v1/message/doesnotexist")
cap("headers_html", "GET", f"/api/v1/message/{H}/headers")
cap("headers_plain", "GET", f"/api/v1/message/{P}/headers")
cap("message_no_msgid", "GET", f"/api/v1/message/{N}")
cap("raw_no_msgid", "GET", f"/api/v1/message/{N}/raw")
cap("raw_dl", "GET", f"/api/v1/message/{P}/raw?dl=1")
for part in ["2", "1.2.2"]:
    cap("part_html_" + part, "GET", f"/api/v1/message/{H}/part/{part}")
cap("message_attachments", "GET", f"/api/v1/message/{A}")
for part in ["2", "3"]:
    cap("part_att_" + part, "GET", f"/api/v1/message/{A}/part/{part}")
    cap("thumb_att_" + part, "GET", f"/api/v1/message/{A}/part/{part}/thumb")
cap("part_not_found", "GET", f"/api/v1/message/{A}/part/99")
cap("view_html", "GET", f"/view/{H}.html")
cap("view_html_embed", "GET", f"/view/{H}.html?embed=1")
cap("view_txt", "GET", f"/view/{H}.txt")
cap("view_html_missing", "GET", f"/view/{P}.html")
cap("info", "GET", "/api/v1/info")
cap("webui", "GET", "/api/v1/webui")
cap("chaos_get_disabled", "GET", "/api/v1/chaos")
cap("tags_get", "GET", "/api/v1/tags")
cap("tags_set_unicode", "PUT", "/api/v1/tags", {"IDs": [P], "Tags": ["فارسی", "ascii-ok"]})
cap("message_after_unicode_tags", "GET", f"/api/v1/message/{P}")
cap("tags_rename", "PUT", "/api/v1/tags/ascii-ok", {"Name": "renamed"})
cap("tags_delete", "DELETE", "/api/v1/tags/renamed")
cap("read_status_ids", "PUT", "/api/v1/messages", {"Read": False, "IDs": [H]})
cap("read_status_both_error", "PUT", "/api/v1/messages", {"Read": True, "IDs": [H], "Search": "x"})
cap("send_api", "POST", "/api/v1/send", {"From": {"Email": "api@example.com", "Name": "API"}, "To": [{"Email": "to@example.com"}],
                                        "Bcc": ["bcc@example.com"], "Subject": "fixture-send", "Text": "hi", "Tags": ["sent"],
                                        "Headers": {"X-Test": "1"}})
cap("send_api_restricted_header", "POST", "/api/v1/send", {"From": {"Email": "api@example.com"}, "To": [{"Email": "to@example.com"}],
                                                          "Subject": "x", "Headers": {"Subject": "override"}})
cap("send_api_bad_json", "POST", "/api/v1/send", b"{not json")
cap("release_not_configured", "POST", f"/api/v1/message/{P}/release", {"To": ["x@example.com"]})
cap("options", "OPTIONS", "/api/v1/messages", None, None)
cap("livez", "GET", "/livez")
cap("readyz", "GET", "/readyz")
cap("delete_search", "DELETE", "/api/v1/search?query=" + urllib.parse.quote("subject:fixture-send"))
cap("delete_ids", "DELETE", "/api/v1/messages", {"IDs": [N]})
cap("delete_invalid_json_body", "DELETE", "/api/v1/messages", b"{oops")
cap("messages_after_deletes", "GET", "/api/v1/messages")

failed = [r for r in results if r[1] == "FAIL"]
for name, status, diffs in results:
    if status != "pass":
        print(f"{name}: {status}")
        for d in diffs:
            print("  " + d.replace("\n", "\n  "))
print(f"{len(results)} cases, {len(failed)} failed" + (" (recorded)" if args.record else ""))
sys.exit(1 if failed else 0)
