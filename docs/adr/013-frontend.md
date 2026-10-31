# ADR-013: Frontend

- وضعیت: **Accepted** (مالک، 2026-10-24)
- تصمیم: **Svelte 5 + TypeScript + Vite** در `web/`؛ خروجی build در باینری embed می‌شود؛ پیش‌نمایش HTML ایمیل در iframe `sandbox` بدون `allow-same-origin` + CSP سرور (`/view/{id}.html`)؛ e2e با Playwright و axe-core؛ پشتیبانی RTL و فونت فارسی از روز اول.
- ردشده: Vue 3 (bundle بزرگ‌تر)، React (سنگین برای این مقیاس)، Rust/WASM (Leptos/Yew — حجم wasm و سرعت توسعه UI).
- پیامدها: Node فقط در زمان build؛ پروژه‌ای که فقط Rust دارد با assetهای از پیش build‌شده در release کامپایل شود.
