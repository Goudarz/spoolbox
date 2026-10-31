# 13. Performance Benchmark Plan

> همه اعداد «هدف» هستند تا اندازه‌گیری شوند. جدول سناریوها (B1–B15) پروتکل کامل اندازه‌گیری است.

## اصول روش‌شناسی

- **سخت‌افزار مرجع:** (H1) لپ‌تاپ توسعه: 8 هسته، 16 GiB، NVMe، Linux؛ (H2) GitHub Actions `ubuntu-latest` (برای regression نسبی، نه مطلق)؛ (H3) Windows 11 و macOS arm64 برای startup/RSS.
- **شرایط ثابت:** همان دیسک، `--max` ثابت، فشرده‌سازی سطح 1، log level `info`؛ هر مقایسه بین نسخه‌ها در همان محیط.
- **تکرار:** هر سناریو ≥ 10 بار؛ گزارش median و p95 بین تکرارها؛ warm-up 1 اجرا دور ریخته می‌شود. CPU governor ثابت (`performance`)، بدون فرایند پس‌زمینه سنگین.
- **ابزارها:** مولد بار SMTP اختصاصی (Rust، در `benches/smtp-load`) با باز-استفاده اتصال قابل تنظیم؛ `swaks` برای تست دستی؛ `hyperfine` برای startup؛ RSS از `/proc/<pid>/status` (VmRSS/VmHWM) نمونه‌برداری 10Hz؛ CPU از `/proc/<pid>/stat`؛ `oha` یا `k6` برای HTTP؛ دیسک کند با `dm-delay` یا `cgroup io.max`.
- **Latency ingest** = زمان از ارسال `.\r\n` تا دریافت `250` در کلاینت. **Latency end-to-end** = تا رسیدن event به کلاینت WebSocket.
- **Fixtures:** تولید deterministic با seed (`xtask fixtures`): S=2 KiB plain؛ H=40 KiB HTML+text؛ A=3 پیوست 200 KiB؛ L=10 MiB و 45 MiB؛ N=MIME تو‌در‌تو عمق 10، 200 part.

## سناریوها

| # | سناریو | داده | بار/تنظیمات | تکرار | اندازه‌گیری | معیار قبولی (هدف پیشنهادی) |
|---|---|---|---|---|---|---|
| B1 | پیام ساده | S | 1 اتصال، 10 000 پیام ترتیبی | 10 | p50/p95/p99 ingest، msg/s | p99 ≤ 5 ms (H1) |
| B2 | HTML | H | مانند B1 | 10 | همان + زمان index | p99 ≤ 8 ms |
| B3 | چند پیوست | A | 4 اتصال، 2 000 پیام | 10 | ingest، حجم دیسک، زمان thumbnail اول | p95 ≤ 25 ms؛ thumbnail p95 ≤ 50 ms |
| B4 | پیام بزرگ | L (10، 45 MiB) | 1 و 8 اتصال هم‌زمان | 10 | peak RSS، زمان ingest | peak RSS ≤ idle + 32 MiB با 8 اتصال 45 MiB (یعنی streaming) |
| B5 | MIME تو‌در‌تو | N | 1 اتصال، 1 000 | 10 | زمان parse، CPU | parse p99 ≤ 20 ms؛ بدون رشد حافظه |
| B6 | Burst | S | 100 اتصال هم‌زمان × 100 پیام | 10 | msg/s، p99، خطاها | 0 خطا؛ p99 ≤ 50 ms |
| B7 | صندوق پر | 100 000 پیام مخلوط (S 70٪، H 25٪، A 5٪) | `--max 0` | 5 | حجم DB، زمان startup با DB پر، لیست صفحه 1 و صفحه 1000 | لیست p95 ≤ 20 ms مستقل از offset (keyset)؛ startup ≤ 300 ms |
| B8 | جست‌وجوی پیچیده | B7 | queryهای مجموعه Q1–Q10 (زیر) | 20 | p50/p95 | Q متنی p95 ≤ 100 ms روی 100k |
| B9 | اتصال‌های هم‌زمان | — | 1000 اتصال idle + 50 فعال | 5 | RSS، p99 فعال‌ها | RSS ≤ 64 MiB |
| B10 | دیسک کند و خطا | S | تأخیر 50 ms/IO؛ ENOSPC تزریقی؛ فایل read-only | 5 | پاسخ SMTP، بازیابی | هیچ `250` بدون commit؛ پس از رفع خطا بدون restart ادامه |
| B11 | Startup | DB خالی و B7 | cold (پس از drop cache) و warm | 20 (hyperfine) | زمان تا `readyz=200` | cold ≤ 150 ms (خالی)؛ warm ≤ 50 ms |
| B12 | Idle | DB خالی | 10 دقیقه | 3 | RSS، CPU٪ | RSS ≤ 15 MiB؛ CPU میانگین ≤ 0.1٪ هسته؛ wakeup ≤ 1/s |
| B13 | Cleanup | B7 → prune به 500 | — | 5 | زمان، قفل نوشتن | ingest هم‌زمان p99 ≤ 50 ms در حین prune |
| B14 | به‌روزرسانی زنده | 50 کلاینت WS + B6 | — | 5 | تأخیر event p99، CPU | p99 ≤ 100 ms؛ کلاینت کند دیگران را کند نکند |
| B15 | باز شدن پیام | H و A از B7 | `GET message` + html view + thumbs | 20 | p95 | ≤ 30 ms |
| B-STOR | blob SQLite در برابر فایل | 50 000 پیام شامل 5٪ 10–50 MiB | هر دو backend | 5 | ingest p95، حذف 10k + بازیابی فضا، حجم | معیار ADR-003 |

**مجموعه Query (B8):** Q1 `invoice`؛ Q2 `"reset your password"`؛ Q3 `from:noreply@ to:user42`؛ Q4 `subject:سفارش`؛ Q5 `has:attachment larger:1m`؛ Q6 `tag:signup is:unread`؛ Q7 `after:2026-01-01 before:2026-02-01 -tag:spam`؛ Q8 `addressed:example.org`؛ Q9 دو حرفی `ab` (fallback LIKE)؛ Q10 ترکیب ۶ token.

## هدف‌های کلی (هنوز اندازه‌گیری نشده)

| معیار | هدف Spoolbox |
|---|---|
| حجم Docker image | ≤ 20 MiB uncompressed |
| Idle RSS | ≤ 15 MiB (اندازه‌گیری‌شده روی Windows: working set 6.2 MiB) |
| Startup (DB خالی) | ≤ 20 ms (اندازه‌گیری‌شده روی Windows: 29 ms) |
| Ingest p99، 1 اتصال | ≤ 10 ms |
| Throughput، 16 اتصال | ≥ 3000 msg/s |
| p99 در 64 اتصال | ≤ 50 ms |
| Peak RSS، 8 × 10 MiB هم‌زمان | ≤ 100 MiB (streaming) |
| Search p95 روی ~28k | ≤ 30 ms |
| Search p95 روی 100k | ≤ 100 ms |
| حجم DB / 28 400 پیام 2 KiB | ≤ 80 MiB با FTS5 (trigram ممکن است بیشتر باشد؛ اندازه‌گیری شود) |

## CI regression
- هر PR: B1، B6 (کوچک‌شده)، B11، B12 (60s) روی H2؛ شکست اگر median > baseline + 15٪ (سه اجرای مجدد برای حذف نویز).
- هفتگی: کل مجموعه روی H1 (self-hosted یا دستی) و انتشار گزارش.
