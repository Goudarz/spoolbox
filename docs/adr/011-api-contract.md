# ADR-011: قرارداد API spec-first

- وضعیت: **Accepted** (2026-10-24)
- تصمیم: `api/openapi-v1.json` (OpenAPI 3.1، ساخته با `tools/api/openapi_v1.py`) و `api/openapi-v2.json` (دست‌نویس)؛ CI: lint، به‌روز بودن v1 با builder، snapshotهای پاسخ (`fixtures/api-v1/`). SDKها (TS/Python) از v2 تولید می‌شوند.
- ردشده: utoipa (code-first تطابق با قرارداد موجود را تضمین نمی‌کند).
