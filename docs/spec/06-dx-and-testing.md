# 14. Developer Experience Improvements و 15. Testing Strategy

## 14. DX — شکاف‌ها و پیشنهادها

شکاف‌های رایج در گردش کار تست ایمیل (نیاز کاربران با بازخورد واقعی تأیید شود):

| ID | پیشنهاد | مشکل کاربر | ارزش | پیچیدگی | نگهداری | ریسک امنیتی | فاز |
|---|---|---|---|---|---|---|---|
| DX-01 | `POST /api/v2/wait` / `spoolbox wait --query ... --timeout 10s` | تست‌ها برای انتظار پیام polling می‌کنند | بسیار بالا | کم (روی event log) | کم | کم | P0 (MVP) |
| DX-02 | Envelope در API و UI | تست Bcc/گیرنده واقعی بدون envelope ممکن نیست | بالا | کم | کم | — | P0 |
| DX-03 | Raw byte-exact | تست DKIM/امضا روی پیام دست‌کاری‌شده نامعتبر است | بالا | کم | کم | — | P0 |
| DX-04 | Event stream با resume (SSE `?since=`) | ازدست‌رفتن event پس از reconnect | بالا | متوسط | متوسط | کم | P1 |
| DX-05 | Namespace منطقی (`user+ns@` یا `X-Spoolbox-Namespace` یا پورت/credential) — P1، درخواست پرتکرار برای CI موازی | جداسازی تست‌های موازی CI | بالا | متوسط | متوسط | کم | P1 |
| DX-06 | `spoolbox doctor` | عیب‌یابی پورت اشغال، TLS، bind | متوسط | کم | کم | — | P1 |
| DX-07 | SMTP transcript (opt-in، با redaction AUTH) | «چرا کلاینت من خطا می‌گیرد؟» | بالا | کم | کم | متوسط (محرمانگی) — پیش‌فرض خاموش | P1 |
| DX-08 | Chaos گسترده و deterministic (`fail-next: 2`، DATA-phase، تأخیر، قطع) | Chaos تصادفی برای تست CI قابل‌تکرار نیست | بالا | کم | کم | — | P1 |
| DX-09 | نمایش درخت MIME و خطاهای parse | parse failure بی‌صدا (MIME-02) | بالا | کم | کم | — | P1 |
| DX-10 | اطلاعات زمان‌بندی (receive→commit→indexed) | — | کم | کم | کم | — | P2 |
| DX-11 | Export/Import (eml، mbox) | fixture گرفتن از تست‌ها | متوسط | متوسط | کم | path traversal | P1 |
| DX-12 | Diff دو پیام (هدر و متن) | مقایسه خروجی دو نسخه template | متوسط | متوسط | متوسط | — | P2 |
| DX-13 | راهنمای framework و نمونه‌ها (Laravel، Django، Rails، Spring، Node، .NET، Go) + snippet docker-compose | — | بالا | کم (مستند) | متوسط | — | P1 |
| DX-14 | کتابخانه‌های کلاینت تست (TS، Python) تولیدشده از OpenAPI v2 | — | متوسط | متوسط | بالا | — | P3 |
| DX-15 | قوانین auto-tag با TOML | — | کم | کم | کم | — | P2 |
| DX-16 | Webhook قابل‌اعتماد (retry، امضا، UI تاریخچه تحویل، ثبت از API، payload خام/parsed) — ارتقا به P1 | drop رویداد | متوسط | متوسط | متوسط | متوسط | P1 |
| DX-17 | TUI | — | کم | متوسط | بالا | — | P3 |
| DX-18 | Expected-vs-received assertion (snapshot template) | — | متوسط | بالا | بالا | — | P3 |
| DX-19 | پیام خطای پیکربندی با پیشنهاد (flag ناشناخته، نزدیک‌ترین نام) | — | متوسط | کم | کم | — | P1 |

**رد شده/به تعویق:** screenshot سمت سرور با headless Chrome (وابستگی سنگین؛ سمت کلاینت انجام می‌شود)؛ IDE plugin (P3)؛ چندکاربره با RBAC (خارج از محصول).

## 15. Testing Strategy

| لایه | دامنه | ابزار | معیار ورود به main |
|---|---|---|---|
| Unit | core، search parser، smtp state machine، mime index | `cargo test` | پوشش خطوط crateهای smtp/mime/search ≥ 85٪ (`cargo llvm-cov`) |
| Integration | store با SQLite موقت؛ app با store واقعی | tempfile | همه سبز روی Linux/Windows/macOS |
| SMTP protocol | transcriptهای `fixtures/smtp/*.txt` (C:/S: خطوط) اجرا روی socket واقعی | runner داخلی | همه transcriptها |
| Client compat | lettre، Python smtplib، Nodemailer، PHPMailer، swaks در Docker | CI هفتگی | ماتریس §16 |
| API contract | پاسخ‌های v1 در برابر snapshotهای `fixtures/api-v1/` + اعتبارسنجی schema OpenAPI | runner + JSON schema | همه endpointهای v1 |
| MIME fixtures | corpus زیر | snapshot (insta یا golden دستی) | بدون diff ناخواسته |
| Fuzz | `smtp_session`، `mime_index`، `search_parse`، `html_rewrite` | cargo-fuzz | 10 دقیقه هر هدف در CI شبانه بدون crash؛ corpus در مخزن |
| Property | search: `parse(print(q)) == q`؛ dot-stuffing: `unstuff(stuff(x)) == x`؛ envelope merge | proptest | — |
| Concurrency | ingest هم‌زمان + delete + prune؛ ترتیب seq | tokio + loom (فقط Hub در صورت نیاز) | بدون deadlock/ترتیب نادرست در 1000 اجرا |
| Crash recovery | kill -9 در نقاط تزریقی (پس از نوشتن blob، قبل/بعد commit، حین job) | harness پروسس فرزند | هیچ `250` بدون پیام؛ هیچ ردیف بدون blob؛ jobها ادامه |
| Storage corruption | صفحه خراب، WAL ناقص، DB read-only، ENOSPC | فایل دست‌کاری‌شده | خطای قابل‌فهم؛ عدم panic؛ `verify` گزارش دهد |
| Security | جدول T9 (SSRF)، corpus XSS در Playwright، header injection، path traversal | — | 100٪ موارد |
| Performance regression | §13 CI | — | ≤ +15٪ |
| Cross-platform | build و تست روی x86_64/aarch64 Linux، Windows، macOS | CI matrix | سبز |
| Migration | DB هر نسخه منتشرشده → نسخه جدید | fixture DBها | داده کامل |
| E2E UI | Playwright: inbox، مشاهده، جست‌وجو، حذف، tag، live update، keyboard | — | سبز |

### Corpus ورودی‌ها

**SMTP:**
- معتبر: EHLO/HELO، PIPELINING دسته کامل، `MAIL FROM:<>`، پارامترهای SIZE/BODY=8BITMIME/SMTPUTF8/AUTH=<>، 100 RCPT، چند تراکنش در یک session، RSET میان تراکنش، STARTTLS سپس EHLO، AUTH PLAIN inline و challenge، LOGIN.
- ناقص: قطع اتصال وسط DATA، وسط AUTH، قبل از QUIT؛ خط بدون CRLF؛ timeout.
- غیرعادی: حروف کوچک (`mail from:`)، فاصله اضافه `MAIL FROM: <a@b>`، local-part نقل‌قول‌دار با فاصله، آدرس 1024 کاراکتری، IDN و آدرس UTF-8، bare LF در DATA، خط > 1000 در DATA.
- مخرب: `\n.\n` / `\r.\r` smuggling، فرمان بعد از `STARTTLS` در همان packet، خط 1 MiB، 10 000 RCPT، base64 نامعتبر در AUTH، NUL در آدرس، `MAIL FROM:<a@b>\r\nRCPT` در یک خط، slowloris.

**MIME:**
- معتبر: plain، HTML، alternative، mixed+related با cid، پیوست با RFC 2231 `filename*=UTF-8''%D9%81...`، Subject RFC 2047 فارسی/عربی/ژاپنی/emoji، QP و Base64، charsetهای windows-1256، iso-8859-6، koi8-r، gb18030، iso-2022-jp، message/rfc822 پیوست‌شده، calendar (text/calendar)، S/MIME و PGP (فقط نمایش).
- ناقص/غیرعادی: boundary گمشده، boundary تکراری، هدر بدون مقدار، هدرهای تکراری (دو Subject)، خطوط folding عجیب، Date نامعتبر، From بدون آدرس، charset ناشناخته، base64 با whitespace/padding نادرست، `\r\r\n`.
- مخرب: عمق 1000، 100 000 part، هدر 10 MiB، filename `../../etc/passwd` و `CON.txt` و RTL override، HTML با `<script>`، `<svg onload>`، `<base href>`، meta refresh، CSS `@import`، تصویر bomb.

### معیارهای قبولی هر فاز
در [07-roadmap.md](07-roadmap.md) برای هر فاز به‌صورت فرمان قابل‌اجرا تعریف شده‌اند (مثال: `cargo xtask test-smtp-transcripts` با 0 شکست).
