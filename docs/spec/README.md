# مشخصات پروژه: ابزار تست ایمیل Rust-Native — Spoolbox

> وضعیت سند: مشخصات زنده؛ وضعیت هر فاز در [07-roadmap.md](07-roadmap.md).
>
> **تصمیم‌های مالک (2026-10-24):** نام **Spoolbox** · MCP و IMAP در v1.0 · Namespace در v1.0 · Rust toolchain = 1.99.1.

## فهرست اسناد

| # | بخش | فایل |
|---|---|---|
| 1–3 | Executive Summary، اصول طراحی، Methodology | همین فایل |
| 8, 16 | قرارداد API v1، سازگاری و flagهای سازگاری | [02-compatibility.md](02-compatibility.md) |
| 9–11, 17 | Data Model، Architecture، Crate Matrix، Repository Structure | [03-architecture.md](03-architecture.md) |
| 12 | Security Threat Model | [04-security.md](04-security.md) |
| 13 | Performance Benchmark Plan | [05-performance.md](05-performance.md) |
| 14–15 | DX Improvements، Testing Strategy | [06-dx-and-testing.md](06-dx-and-testing.md) |
| 18–24 | Roadmap، Tasks، Risks، MVP، Acceptance، ADRها، نقطه توقف | [07-roadmap.md](07-roadmap.md) |

---

## 1. Executive Summary

**Spoolbox** یک ابزار تست ایمیل برای توسعه‌دهندگان است: یک باینری Rust که SMTP سرور، API، رویدادهای زنده و UI وب را ارائه می‌دهد تا ایمیل‌های ارسالی یک برنامه در محیط توسعه و CI گرفته، دیده و در تست‌های خودکار بررسی شوند.

**مشکلات رایج ابزارهای این دسته که Spoolbox برای حل آن‌ها طراحی شده:**

1. **از دست رفتن envelope SMTP**: اگر گیرندگان envelope جدا ذخیره نشوند و به‌جایش در هدرها نوشته شوند، پیام ذخیره‌شده دیگر همان پیام دریافتی نیست و Bccها ممکن است گم شوند.
2. **جست‌وجوی O(N)**: بار کردن همه نتایج در حافظه و صفحه‌بندی در برنامه، با `LIKE '%x%'` بدون ایندکس.
3. **کار همگام در مسیر SMTP**: relay/forward پیش از پاسخ DATA، کلاینت را معطل می‌کند و retry ندارد.
4. **رویدادهای زنده غیرقابل‌اتکا**: بدون شماره ترتیب و replay، کلاینتی که قطع و وصل می‌شود رویداد از دست می‌دهد؛ webhook بدون retry.
5. **پیش‌فرض‌های شبکه‌ای باز**: گوش دادن روی همه interfaceها و تماس خودکار با اینترنت.
6. **Unicode محدود**: tag غیر ASCII (فارسی/عربی) پذیرفته نمی‌شود.
7. **عوارض جانبی خطرناک**: حذف همه پیام‌ها با body نامعتبر.

**طراحی:** یک **modular monolith** تک‌باینری با Rust stable (tokio + axum + rustls + rusqlite/SQLite WAL + FTS5):
- ذخیره **envelope جدا از پیام خام** و نگهداری پیام خام بدون تغییر (byte-exact).
- **ingest دو مرحله‌ای**: پاسخ `250` فقط پس از commit پایدار؛ parse کامل، indexing، relay و webhook به صورت job پس‌زمینه با outbox پایدار و retry.
- **Event log ترتیبی** برای WebSocket/SSE با resume پس از reconnect.
- **جست‌وجو با FTS5 (trigram)** + ستون‌های ساخت‌یافته.
- **پیش‌فرض امن**: bind به `127.0.0.1`، بدون تماس خروجی، Host allowlist.
- **API v1 پایدار** برای UI و کلاینت‌های تست، و **API v2** برای قابلیت‌های جدید (wait، SSE، envelope، استخراج کد).

**ادعای عملکردی نمی‌کنیم** مگر با اندازه‌گیری؛ هدف‌ها در [05-performance.md](05-performance.md).

## 2. اصول

| اصل | معنی |
|---|---|
| دوام | هیچ پیامی پس از `250` گم نمی‌شود (crash test با ۱۰۰۰ kill) |
| وفاداری | پیام خام بایت‌به‌بایت؛ envelope جدا |
| امنیت پیش‌فرض | loopback، بدون egress، CSP سخت، دانلود امن پیوست‌ها |
| سازگاری | flagهای خط فرمان و متغیرهای `MP_*` رایج پذیرفته می‌شوند تا جایگزینی بی‌دردسر باشد؛ هر گزینه پشتیبانی‌نشده صریحاً گزارش می‌شود |
| چندزبانه | Unicode در tag، جست‌وجو، فایل‌ها و UI (RTL) از روز اول |

## 3. Methodology

1. هر رفتار API با تست یکپارچه و snapshot (`fixtures/api-v1/`، `tools/snapshot/api_v1.py`) قفل می‌شود.
2. نسخه crateها از API رسمی crates.io و با قاعده سن ≥ ۱۴ روز (ADR-015) انتخاب می‌شوند.
3. toolchain: Rust 1.99.1 (`rust-toolchain.toml`)، Windows MSVC و Linux در CI.
4. RFCها با شماره بخش ارجاع داده می‌شوند.

---

## 18. نقطه توقف و تأیید

جزئیات در [07-roadmap.md §18](07-roadmap.md#18-نقطه-توقف-و-تأیید). تصمیم‌های تأییدشده:

1. **D1** معماری modular monolith تک‌باینری (ADR-001).
2. **D2** پیام خام در SQLite blob (ADR-003).
3. **D3** پیش‌فرض bind به `127.0.0.1`؛ Docker image روی `0.0.0.0` (ADR-008).
4. **D4** `/api/v1` پایدار + `/api/v2` جدید (ADR-010).
5. **D5** مجوز `MIT OR Apache-2.0` (ADR-012).
6. **D6** frontend: Svelte + TypeScript + Vite (ADR-013).
7. **D7** بدون rqlite و tenant-prefix؛ namespace به‌جای tenant (ADR-016).
