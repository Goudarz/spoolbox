# ADR-002: tokio + axum + rustls

- وضعیت: **Accepted** (2026-10-24)
- تصمیم: tokio 1.54.x، axum 0.8.x، tower-http 0.7.x، rustls 0.23.x با provider **`ring`** (روی ماشین توسعه `cmake` نیست و `aws-lc-rs` به آن نیاز دارد)، tokio-rustls 0.26.x. WebSocket از `axum::extract::ws`.
- ردشده: native-tls (رفتار متفاوت بین سیستم‌عامل‌ها)، actix-web (اکوسیستم tower کوچک‌تر برای ما).
- پیامدها: axum 0.x است؛ ارتقای 0.9 در برنامه نگه‌داری.
