# 12. Security Threat Model

## دارایی‌ها و مرزهای اعتماد

- **دارایی‌ها:** محتوای ایمیل‌های تست (ممکن است شامل داده واقعی کاربران، توکن reset password، OTP باشد)، credentialهای relay/SMTP upstream، سیستم فایل میزبان، شبکه داخلی میزبان (هدف SSRF)، مرورگر توسعه‌دهنده (هدف XSS).
- **مهاجمان:** (M1) فرستنده ایمیل (هر کسی که به پورت SMTP دسترسی دارد یا اپلیکیشن تحت تست را به ارسال وادار می‌کند — محتوای ایمیل **غیرقابل‌اعتماد** است)؛ (M2) وب‌سایت مخرب در مرورگر توسعه‌دهنده (CSRF، DNS rebinding)؛ (M3) کاربر دیگر در شبکه مشترک/LAN؛ (M4) کانتینر دیگر در همان شبکه Docker.
- **مرزها:** socket SMTP/POP3 ← parser؛ HTTP API ← مرورگر؛ HTML ایمیل ← iframe؛ سرور ← اینترنت (link check، webhook، relay).

## تهدیدها و کنترل‌ها

| # | تهدید | کنترل پیشنهادی | تست/معیار |
|---|---|---|---|
| T1 | bind ناخواسته به شبکه | **پیش‌فرض `127.0.0.1`/`::1`**؛ bind غیر-loopback بدون auth یا `--allow-unauthenticated-network` → خطای startup؛ Docker image صراحتاً `0.0.0.0` با پیام بنر | تست startup برای هر ترکیب |
| T2 | DNS rebinding علیه API | allowlist پیش‌فرض: `localhost`، `127.0.0.1`، `[::1]`، نام‌های bind؛ هر Host دیگر → 403 | تست Host header |
| T3 | CSRF (سایت مخرب `DELETE` می‌زند) | الزام `Content-Type: application/json` یا هدر سفارشی برای متدهای تغییر‌دهنده؛ بررسی `Origin`/`Sec-Fetch-Site` | تست form POST cross-origin → 403 |
| T4 | Clickjacking UI | `frame-ancestors 'self'` | هدر در همه پاسخ‌ها |
| T5 | Stored XSS از HTML ایمیل | حفظ همین + هدر `Content-Security-Policy: sandbox; default-src 'none'; img-src ...; style-src 'unsafe-inline' ...` روی `/view/*.html` و partهای HTML؛ `X-Content-Type-Options: nosniff` همه‌جا | corpus XSS (OWASP cheat sheet، mXSS، SVG، `<base>`، meta refresh، `javascript:` در href، CSS `expression`) |
| T6 | ردیابی/نشت از تصاویر remote | گزینه UI «بارگذاری تصاویر remote» با پیش‌فرض **روشن** برای سازگاری؟ — **تصمیم مالک** (Q4) | — |
| T7 | دانلود فایل خطرناک | مشابه + `nosniff` + `filename*` (RFC 8187)؛ پاک‌سازی نام: حذف `/ \ : * ? " < > |`، کنترل‌ها، bidi override (U+202E)، نام‌های رزرو Windows | fixture نام مخرب |
| T8 | Path traversal | blob با نام hash؛ export/dump با sanitization؛ canonicalize و بررسی prefix | تست `../` در filename هنگام export |
| T9 | SSRF در link check / proxy / CSS fetch | همان سیاست به‌صورت **ماژول مستقل `egress`**: فقط http/https؛ پورت‌های 80/443 پیش‌فرض (قابل تنظیم)؛ resolve → فیلتر همه IPها → pin؛ هر redirect دوباره ارزیابی؛ حداکثر 3 redirect؛ timeout کل 10s؛ سقف body (proxy 50 MiB → پیشنهاد 10 MiB)؛ بدون ارسال cookie/credential؛ IPv6 transition forms | جدول تست IPها (127/8، 10/8، 169.254.169.254، `::ffff:127.0.0.1`، `64:ff9b::a9fe:a9fe`، `0.0.0.0`، DNS با چند A که یکی خصوصی) |
| T10 | DNS rebinding در SSRF | همان | resolver تستی با پاسخ متغیر |
| T11 | MIME مخرب (عمق، تعداد part، پیام بزرگ) | سقف عمق 32، 500 part، سقف هدر 1 MiB، سقف تعداد هدر 10 000، timeout parse 5s، اجرای parse در blocking pool محدود | fuzz + fixture «zip-bomb-like» base64 تودرتو |
| T12 | Image bomb در thumbnail | `image::Limits` (max 40 MP، max_alloc 256 MiB)، thumbnail در blocking pool، cache | fixture PNG 50000×50000 |
| T13 | فایل فشرده/archive bomb | هیچ پیوستی decompress نمی‌شود (فقط zstd داخلی با سقف خروجی = size ذخیره‌شده) | — |
| T14 | تزریق هدر | رد CR/LF در همه مقادیر هدر ورودی API؛ builder تنها مسیر ساخت | تست `\r\n` در Subject/Name |
| T15 | SMTP command injection / smuggling | state machine صریح؛ terminator فقط CRLF.CRLF؛ PIPELINING با buffer مستقل؛ بعد از STARTTLS buffer خالی (حمله STARTTLS command injection، CVE-2011-0411 class) | تست تزریق فرمان قبل از handshake |
| T16 | سوءاستفاده از relay (open relay) | پیش‌فرض خاموش؛ نیاز به allowlist صریح برای `relay-all`؛ rate limit؛ log هر ارسال | — |
| T17 | Webhook ناامن | HMAC-SHA256 (`X-Signature`، timestamp)؛ فقط https یا loopback به‌طور پیش‌فرض؛ timeout 5s | — |
| T18 | Secret در log | redaction: هرگز password/AUTH payload را log نکن (حتی در debug transcript)؛ `Secret<String>` type | grep لاگ تست برای رمز نمونه |
| T19 | Brute force | محدودیت تلاش ناموفق per-IP (SMTP AUTH، Basic، POP3) با تأخیر | — |
| T20 | DoS اتصال/حافظه | stream DATA؛ سقف هم‌زمانی DATA فعال (مثلاً 64) جدا از اتصالات؛ سقف کل حافظه buffer | تست 1000 اتصال slowloris |
| T21 | تماس شبکه خروجی ناخواسته | پیش‌فرض خاموش | تست بدون شبکه |
| T22 | گواهی و رمز | پشتیبانی bcrypt/sha-crypt؛ هشدار برای فرمت ضعیف؛ reload با SIGHUP (P2) | — |

## پروفایل‌های امنیتی

| پروفایل | bind | Auth | توضیح |
|---|---|---|---|
| `local` (پیش‌فرض باینری) | loopback | ندارد | امن برای توسعه محلی |
| `container` (پیش‌فرض Docker) | `0.0.0.0` | ندارد | شبکه کانتینر مرز اعتماد است؛ بنر هشدار |
| `shared` | هر | **الزامی** (UI + SMTP) | Host allowlist، TLS توصیه، rate limit |

## الزامات قبل از پیاده‌سازی قابلیت‌های پرریسک
- **link check/proxy/htmlcheck CSS fetch** (P2): تست‌های جدول T9 باید **قبل** از کد handler نوشته شوند.
- **relay/forward**: تست open-relay و redaction قبل از ادغام.
- **HTML preview**: corpus XSS با Playwright (assert: هیچ `alert`/network به دامنه مهاجم) قبل از release UI.
