# ADR-010: نسخه‌بندی API

- وضعیت: **Accepted** (2026-10-24)
- تصمیم:
  - `/api/v1`: API پایدار برای UI و کلاینت‌های تست؛ قرارداد: `api/openapi-v1.json`؛ snapshotها: `fixtures/api-v1/`.
  - رفتارهای امن: `DELETE /api/v1/messages` با JSON نامعتبر → 400 (هرگز حذف همه)؛ raw بدون هدر تزریقی؛ tag Unicode پذیرفته؛ `Content-Disposition` با `filename*` (RFC 8187).
  - `/api/v2`: **در MVP حداقلی و با برچسب `preview`** — `wait`، `envelope`، `mime`، `links`، `codes`، `events` (SSE). **تصمیم مالک: v2 در نسخه‌های بعدی کامل و جامع می‌شود**؛ تا v1.0 تغییر ناسازگار در v2 مجاز است و پس از آن قفل.
- پیامدها: roadmap v2 کامل در P6/P7 (فهرست در spec 07).
