# ADR-007: جست‌وجو

- وضعیت: **Accepted** (2026-10-24)
- زمینه: بار کردن همه نتایج در حافظه و صفحه‌بندی در برنامه با رشد صندوق کند می‌شود؛ مقدار هدرهای سفارشی (مثل `X-Custom-Id`) باید قابل جست‌وجو باشد.
- تصمیم: crate `search` (lexer → AST → SQL پارامتری)؛ گرامر فیلترها + `header:Name=value`؛ FTS5 با tokenizer `trigram` (term < 3 کاراکتر → LIKE)؛ keyset pagination روی `(received_at, seq)` + سازگاری offset برای v1؛ case-fold Unicode در index.
- ردشده: tantivy (index جدا، هماهنگی crash، حجم).
- پیامدها: حجم index بیشتر (اندازه‌گیری در B7)؛ OR/پرانتز در P2.
