# SMTP transcripts

Client/server conversations in the format documented in `crates/testkit/src/transcript.rs`.
Each file runs on a fresh connection. Files here describe plain RFC 5321 behaviour,
so they pass against any conforming server; reply texts are matched loosely (`*`), codes exactly.

Run against any server:

```
SPOOLBOX_TEST_SMTP_ADDR=127.0.0.1:1025 cargo test -p spoolbox-testkit --test transcripts
```

