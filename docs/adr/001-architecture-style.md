# ADR-001: Modular monolith

- وضعیت: **Accepted** (مالک، 2026-10-24)
- زمینه: وابستگی دوطرفه بین ماژول‌ها (مثلاً ذخیره‌سازی که مستقیماً broadcast و webhook را صدا بزند) نگهداری و تست را سخت می‌کند. مقایسه سه گزینه در [spec 03 §10](../spec/03-architecture.md).
- تصمیم: یک باینری و یک runtime؛ crateهای workspace با مرز trait (`core`، `smtp`، `mime`، `search`، `store`، `app`، `http`، `integrations`، `bin`). کار CPU-bound روی `spawn_blocking` با Semaphore.
- ردشده: برنامه یکپارچه بدون مرز (نگه‌داری)؛ workerهای پروسس جدا (RAM و پیچیدگی deploy؛ Rust خطر حافظه parser را کم می‌کند).
- پیامدها: تست هر crate بدون شبکه/DB واقعی؛ امکان جداسازی پروسس افزونه‌های پرریسک در آینده بدون بازطراحی.
