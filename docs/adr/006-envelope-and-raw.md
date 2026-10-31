# ADR-006: Envelope جدا و پیام خام دست‌نخورده

- وضعیت: **Accepted** (2026-10-24)
- زمینه: نوشتن envelope در هدرهای پیام (`Return-Path`، `Received`، `Bcc`، `Message-ID` ساختگی) پیام خام را تغییر می‌دهد و در ادغام هدر Bcc موجود، گیرنده‌های envelope ممکن است گم شوند.
- تصمیم: envelope (MAIL FROM، RCPT TOها، پارامترها، HELO، IP، TLS، auth user) در جداول `envelopes`/`recipients`؛ پیام خام بایت‌به‌بایت؛ v1 API فیلد `Bcc` را از (هدر ∪ envelope غایب) می‌سازد.
- پیامدها: تست DKIM روی raw ممکن؛ v2 نام و ترتیب اصلی هدرها را حفظ می‌کند.
