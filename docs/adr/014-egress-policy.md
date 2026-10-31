# ADR-014: سیاست egress (SSRF)

- وضعیت: **Accepted** (2026-10-24)
- تصمیم: ماژول مستقل `egress` برای همه درخواست‌های خروجی سرور (link check، proxy screenshot، CSS fetch در HTML check، webhook غیر-loopback): فقط http/https؛ resolve → رد اگر **هر** IP داخلی/رزرو باشد → pin IP؛ ارزیابی مجدد هر redirect (حداکثر 3)؛ timeout کل 10s؛ سقف body؛ بدون cookie/credential. link check: HEAD سپس GET با `Range: bytes=0-0` در 405/501.
- پیامدها: جدول تست T9 ([spec 04](../spec/04-security.md)) پیش از نوشتن هر handler خروجی.
