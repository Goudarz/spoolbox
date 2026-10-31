# Architecture Decision Records — Spoolbox

قالب هر ADR: وضعیت، تاریخ، زمینه، تصمیم، گزینه‌های ردشده، پیامدها. تصمیم‌های مالک در 2026-10-24 ثبت شدند.

| ADR | عنوان | وضعیت |
|---|---|---|
| [001](001-architecture-style.md) | Modular monolith | Accepted |
| [002](002-runtime-http.md) | tokio + axum + rustls | Accepted |
| [003](003-raw-message-storage.md) | ذخیره پیام خام در SQLite پشت `BlobStore` | Accepted (benchmark-gated) |
| [004](004-database.md) | SQLite (rusqlite bundled، WAL) | Accepted |
| [005](005-smtp-250-semantics.md) | `250` فقط پس از commit | Accepted |
| [006](006-envelope-and-raw.md) | Envelope جدا، raw بایت‌به‌بایت | Accepted |
| [007](007-search.md) | Parser مستقل + FTS5 trigram + keyset | Accepted |
| [008](008-network-defaults.md) | پیش‌فرض loopback و بدون egress | Accepted |
| [009](009-events-outbox.md) | Event log ترتیبی + outbox | Accepted |
| [010](010-api-versioning.md) | v1 سازگار + v2 | Accepted |
| [011](011-api-contract.md) | OpenAPI spec-first | Accepted |
| [012](012-license.md) | MIT OR Apache-2.0، clean-room | Accepted |
| [013](013-frontend.md) | Svelte 5 + TypeScript + Vite | Accepted |
| [014](014-egress-policy.md) | سیاست egress (SSRF) | Accepted |
| [015](015-toolchain-dependencies.md) | Toolchain 1.99.1 و سیاست وابستگی | Accepted |
| [016](016-scope-decisions.md) | تصمیم‌های دامنه (rqlite/tenant، CRAM-MD5، POP3، SpamAssassin، Windows، تصاویر remote، سقف‌ها) | Accepted |
