# ADR-004: SQLite

- وضعیت: **Accepted** (2026-10-24)
- تصمیم: rusqlite (feature `bundled` برای FTS5)؛ WAL؛ `synchronous=NORMAL`؛ یک اتصال writer + pool خواننده؛ هر migration در یک تراکنش با checksum؛ DB موقت پیش‌فرض و `--database` برای پایدار.
- ردشده: sqlx (macro و async بی‌فایده برای SQLite)، redb/fjall (نبود FTS و query ad-hoc)، rqlite (ADR-016).
- پیامدها: کامپایل C با MSVC (موجود: VS 2026)؛ cross-compile با zig/cross در CI.
