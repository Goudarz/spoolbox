# ADR-003: ذخیره پیام خام

- وضعیت: **Accepted — benchmark-gated** (2026-10-24)
- زمینه: پیام‌ها باید فشرده و تراکنشی ذخیره شوند؛ backup دیتابیس چندگیگابایتی نباید سخت باشد.
- تصمیم: پیش‌فرض **blob در SQLite** (zstd سطح 1، incremental blob I/O) پشت trait `BlobStore`. پیاده‌سازی فایل‌محور فقط اگر benchmark `B-STOR` یکی از این‌ها را نشان دهد: p95 ingest در SQLite > 2× حالت فایل، یا حذف 10 000 پیام + بازیابی فضا > 10s.
- ردشده (فعلاً): فقط فایل (اتمی‌بودن دوگام، Windows file locking)؛ هیبرید از روز اول (پیچیدگی پیش از اثبات نیاز).
- پیامدها: backup یک فایل (`VACUUM INTO`)؛ `auto_vacuum=INCREMENTAL`؛ فرمان `export` برای کاهش نیاز به backup DB.
