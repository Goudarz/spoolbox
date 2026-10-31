# HTML preview corpus

Messages with script, event-handler, `javascript:`, `<base>`, meta refresh, form and mXSS payloads.
`web/e2e/xss.spec.ts` opens each one and fails if any payload runs (`window.__xss` or a dialog)
in the UI or escapes the sandboxed preview frame.
