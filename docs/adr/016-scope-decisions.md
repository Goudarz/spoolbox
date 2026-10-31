# ADR-016: تصمیم‌های دامنه

- وضعیت: **Accepted** (مالک، 2026-10-24)

| موضوع | تصمیم |
|---|---|
| rqlite و `--tenant-id` | حذف؛ namespace (v1.0) جایگزین tenant. استفاده از flag → خطای صریح «not supported» |
| CRAM-MD5 | حذف (در صورت درخواست کاربران بعداً) |
| POP3 | در v1.0، فاز P7، با flagهای `--pop3-*` + STLS اختیاری |
| SpamAssassin | بعد از v1.0، افزونه اختیاری؛ فقط `spamd` محلی، **بدون** ارسال به API خارجی (Postmark) |
| Windows | پشتیبانی رسمی در MVP؛ CI روی Windows/Linux/macOS از Phase 1؛ Unix socket فقط Linux/macOS |
| تصاویر remote | پروفایل `local`: بارگذاری خودکار + دکمه یک‌کلیکی مسدودسازی (به خاطر سپرده می‌شود) + گزارش tracking pixel؛ پروفایل `shared`: مسدود پیش‌فرض |
| سقف‌ها | `max-messages=500`، `max-message-size=50MiB`، `max-age` خاموش، **محافظ دیسک**: فضای آزاد < 1 GiB → `452 4.3.1` |
| MCP، IMAP، namespace | در v1.0 (MCP در P6، IMAP و namespace در P7؛ schema namespace از P3) |
| نام | Spoolbox (`spoolbox`، `SPOOLBOX_`) |
