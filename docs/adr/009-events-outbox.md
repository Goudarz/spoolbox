# ADR-009: Event log و outbox

- وضعیت: **Accepted** (2026-10-24)
- زمینه: ارسال هر broadcast به‌صورت ناهمگام و جدا ترتیب را تضمین نمی‌کند؛ حذف webhookهای اضافی و نبود replay یعنی کلاینت کند یا قطع‌شده رویداد از دست می‌دهد.
- تصمیم: جدول `events` با `seq` یکنواخت در همان تراکنش تغییر؛ Hub پس از commit اطلاع می‌دهد؛ WebSocket سازگار v1 + SSE v2 با `?since=seq`؛ کلاینت کند به‌جای قطع، از DB catch-up می‌کند؛ outbox پایدار برای webhook/relay/forward با backoff و idempotency key.
- پیامدها: `wait` (DX-01) و MCP روی همین log ساخته می‌شوند.
