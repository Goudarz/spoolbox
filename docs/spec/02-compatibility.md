# قرارداد API v1 و سازگاری

قرارداد رسمی: [`api/openapi-v1.json`](../../api/openapi-v1.json) (ساخته‌شده با `tools/api/openapi_v1.py`). رفتار واقعی با تست‌های یکپارچه (`crates/http/src/api_tests.rs`) و snapshotها (`fixtures/api-v1/`، `tools/snapshot/api_v1.py`، job `API v1 snapshots` در CI) قفل شده است.

## 8. API v1 (`/api/v1`)

نکات مشترک:
- نام فیلدهای JSON به‌صورت **PascalCase** (`ID`، `MessageID`، `From`، ...)، به‌جز خلاصه لیست که snake_case است (`total`، `unread`، `messages_count`، `messages_unread`، `start`، `tags`، `messages` و فیلد legacy `count`). نام فیلدهای ورودی بدون حساسیت به حروف خوانده می‌شوند (`{"read": true}`).
- آدرس: `{"Name": "...", "Address": "..."}`؛ در خلاصه لیست، `To`/`Cc`/`Bcc` خالی برابر `null` و در پیام کامل `[]`؛ `ReplyTo` خالی در `/messages` برابر `[]` و در `/search` برابر `null` (کلاینت‌های موجود به این شکل وابسته‌اند).
- خطا: `400` با `text/plain`؛ Send API خطای JSON `{"Error": "..."}` و `413` برای اندازه. 404: «404 page not found».
- Body درخواست‌ها حداکثر 5 MiB (به‌جز send که `--max-message-size`).
- Pagination: `start` (پیش‌فرض 0)، `limit` (پیش‌فرض 50؛ `0` = بدون محدودیت)، `before` (تاریخ)، `tz`.
- `latest` به‌جای ID (با `?query=` اختیاری) در message، headers، raw، part و view.
- Auth: Basic در صورت `--ui-auth-file`؛ Send API با credentials جدا یا بدون رمز؛ CORS (`--api-cors`) و Host allowlist.

| # | Method | Path | رفتار |
|---|---|---|---|
| 1 | GET | `/api/v1/messages` | لیست، جدیدترین اول |
| 2 | PUT | `/api/v1/messages` | read/unread با `IDs`، با `Search`، یا (هیچ‌کدام) همه؛ هر دو با هم → 400 |
| 3 | DELETE | `/api/v1/messages` | `IDs`؛ body خالی یا `{}` → حذف همه؛ **JSON نامعتبر → 400** (هرگز حذف همه) |
| 4 | GET | `/api/v1/search` | `query` الزامی؛ `messages_count`/`messages_unread` = منطبق‌ها |
| 5 | DELETE | `/api/v1/search` | حذف منطبق‌ها |
| 6 | POST | `/api/v1/send` | ساخت پیام از JSON؛ آدرس به‌صورت شیء یا رشته (`Name <a@b>`)؛ هدرهای محدود رد می‌شوند |
| 7–10 | GET/PUT/PUT/DELETE | `/api/v1/tags`، `/api/v1/tags/{tag}` | لیست، جایگزینی tagهای پیام (Unicode مجاز)، rename، delete |
| 11 | GET | `/api/v1/message/{id}` | پیام کامل؛ read می‌شود؛ پیوست‌ها با MD5/SHA1/SHA256 |
| 12 | GET | `/api/v1/message/{id}/headers` | نام canonical (`Message-Id`)، مقدار خام |
| 13 | GET | `/api/v1/message/{id}/part/{part}` | محتوای decode‌شده؛ HTML/XML/SVG همیشه `application/octet-stream` + attachment؛ `filename*` (RFC 8187) |
| 14 | GET | `.../part/{part}/thumb` | JPEG 180×120 (سفید برای غیرتصویر) |
| 15 | GET | `/api/v1/message/{id}/raw` | بایت‌به‌بایت همان پیام دریافتی؛ `dl=1` |
| 16 | POST | `/api/v1/message/{id}/release` | بدون relay: 400 «message relay is not configured»؛ با relay: بررسی گیرنده‌ها، صف‌کردن و پاسخ `ok`؛ ارسال در پس‌زمینه و با retry (تفاوت عمدی: خطای SMTP در پاسخ برنمی‌گردد)؛ فقط `application/json` |
| 17 | GET | `html-check` | `501` تا P7 |
| 18 | GET | `link-check` | وضعیت لینک‌ها (`follow=true` تا ۳ redirect)؛ آدرس داخلی و پورت غیر 80/443 رد می‌شود مگر با `--allow-internal-http-requests`؛ با `--disable-link-check` مقدار `501` |
| 20 | GET | `/api/v1/info` | `LatestVersion: "disabled"` (بدون تماس با اینترنت)؛ `Memory` فقط Linux |
| 21 | GET | `/api/v1/webui` | پیکربندی UI |
| 22 | GET/PUT | `/api/v1/chaos` | 400 «Chaos is not enabled» مگر `--enable-chaos` |
| 23 | GET | `/view/{id}.html` | `cid:` → لینک part؛ حذف `<base>` و meta refresh؛ CSP با `sandbox`؛ `embed=1`: لینک‌ها در تب جدید + script ارتفاع با nonce |
| 24 | GET | `/view/{id}.txt` | متن |
| — | GET | `/api/v1/swagger.json` | `api/openapi-v1.json` |
| — | GET | `/api/events` | WebSocket: `new`، `update`، `delete`، `truncate`، `prune`، `stats` |
| — | — | `/livez`، `/readyz`، `OPTIONS /api/v1/*` | — |

### API v2 (preview)
- JSON snake_case، خطای RFC 9457 (`application/problem+json`)، بدون side effect در GET.
- `POST /api/v2/wait`، `GET /api/v2/events?since=` (SSE با `Last-Event-ID` و `resync`)، `GET /api/v2/messages/{id}/envelope|mime|links|codes`.
- OpenAPI 3.1 دست‌نویس در `api/openapi-v2.json`.

## 16. سازگاری و مهاجرت

| حوزه | وضعیت | توضیح |
|---|---|---|
| SMTP client compatibility | Python smtplib آزموده؛ بقیه در P8 | PHPMailer، Symfony Mailer، Laravel، Nodemailer، Django، Rails، Go net/smtp، Jakarta Mail، MailKit، swaks، curl |
| رفتار پروتکل | — | PIPELINING اعلام می‌شود؛ EHLO مجدد پس از STARTTLS لازم؛ CRAM-MD5 پشتیبانی نمی‌شود |
| فرمت پیام خام | byte-exact | envelope جدا از `/api/v2/messages/{id}/envelope` |
| Search syntax | کامل | `from:`، `to:`، `cc:`، `bcc:`، `reply-to:`، `addressed:`، `subject:`، `message-id:`، `tag:`، `is:`، `has:`، `after:`/`before:`، `larger:`/`smaller:`، `header:Name=value`، نفی با `-`/`!` |
| CLI flags | کامل | flagهای رایج این دسته ابزار با همان نام پذیرفته می‌شوند؛ deprecated با هشدار؛ پشتیبانی‌نشده‌ها صریحاً گزارش یا رد می‌شوند (registry در `crates/config/src/registry.rs`) |
| Environment variables | کامل | متغیرهای `MP_*` به‌عنوان alias؛ prefix بومی `SPOOLBOX_*` |
| Docker | — | پورت‌های 1025/8025، image `scratch`، کاربر غیر root، HEALTHCHECK با `readyz` |
| rqlite، tenant | پشتیبانی نمی‌شود | namespace جایگزین (ADR-016) |

### مسیر مهاجرت کاربر
1. جایگزینی باینری/ایمیج با همان flagها، متغیرها و پورت‌ها؛ هر گزینه پشتیبانی‌نشده خطا یا هشدار صریح می‌دهد (هرگز نادیده‌گرفتن بی‌صدا).
2. فرمان `spoolbox compat-report` (برنامه‌ریزی‌شده) برای تحلیل flagها و متغیرهای فعلی.
