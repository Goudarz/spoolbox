# ADR-008: پیش‌فرض‌های شبکه

- وضعیت: **Accepted** (2026-10-24)
- زمینه: ابزار تست ایمیل روی لپ‌تاپ توسعه‌دهنده اجرا می‌شود؛ گوش دادن روی همه interfaceها یا نبود هدرهای `X-Content-Type-Options` و `frame-ancestors` آن را در شبکه عمومی آسیب‌پذیر می‌کند.
- تصمیم: باینری روی `127.0.0.1`/`::1`؛ Docker image روی `0.0.0.0` با بنر هشدار؛ bind غیر-loopback بدون auth → خطای startup مگر `--allow-unauthenticated-network`؛ Host allowlist پیش‌فرض؛ `nosniff` و `frame-ancestors 'self'` همه‌جا؛ هیچ اتصال خروجی پیش‌فرض (بدون بررسی نسخه).
- پیامدها: در راهنمای مهاجرت ذکر شود که دسترسی از شبکه نیاز به auth یا flag صریح دارد.
- **تکمیل در P1 (2026-10-24) — Host allowlist پیش‌فرض:** حمله DNS rebinding به نام دامنه عمومی تحت کنترل مهاجم نیاز دارد که همیشه نقطه دارد. بنابراین بدون پیکربندی مجازند: IP literal، `localhost` و `*.localhost` (RFC 6761)، و نام‌های تک‌بخشی (مثل `spoolbox` — نام سرویس در Docker Compose). سایر نام‌ها → `403 Blocked due to disallowed Host header` مگر در `--allowed-hosts` (`*` = همه). `/livez` و `/readyz` معاف‌اند. پیاده‌سازی: `crates/http/src/host.rs`.
