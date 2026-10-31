# Spoolbox

Email and SMTP testing tool for developers, written in Rust. Spoolbox captures mail sent by your
application, shows it in a web UI and exposes an API for automated tests. Common command-line flags
and `MP_*` environment variables are accepted, so it can replace an existing setup without changes; see
[`docs/spec/02-compatibility.md`](docs/spec/02-compatibility.md).

> **Status: pre-release, phases 1–4 of the [roadmap](docs/spec/07-roadmap.md) are done.** Spoolbox receives
> mail over SMTP (STARTTLS, implicit TLS, AUTH PLAIN/LOGIN with htpasswd credentials or accept-any,
> PIPELINING, SMTPUTF8), stores it durably in SQLite and serves API v1
> (`/api/v1/...`, `/view/{id}.html`, WebSocket `/api/events`) plus a v2 preview (`/api/v2/wait`, SSE events,
> envelope, MIME tree, links, codes). A web UI (Svelte) is being added in phase 5 and is embedded in the binary.

## Design

- `250 Ok` is only sent after the message is committed to disk; a crash never loses an acknowledged message.
- The stored message is exactly what was received. The SMTP envelope (including Bcc recipients) is kept
  separately instead of being written into the message headers.
- Listens on loopback by default; never contacts the internet on its own.
- Incoming messages are streamed to disk while they are received instead of being buffered in memory
  (the background MIME indexer still parses one whole message at a time; memory use for very large
  messages has not been benchmarked yet).
- Tags may use any language.
- `DELETE /api/v1/messages` with a body that is not valid JSON returns 400 and never deletes anything.
- `/view/{id}.html` is served with a sandboxing Content-Security-Policy; HTML, XML and SVG parts are always
  downloaded, never rendered; no update checks or other outgoing requests.

API v1 responses are locked by snapshot tests ([`tools/snapshot/api_v1.py`](tools/snapshot/api_v1.py),
[`fixtures/api-v1/`](fixtures/api-v1/)) that run in CI.

## API

- **v1**: messages, search, tags, message/part/raw/headers/thumbnails, send, info,
  webui, chaos, swagger — see [`api/openapi-v1.json`](api/openapi-v1.json).
- **v2 preview**: `POST /api/v2/wait` blocks until a matching message arrives, which is the easiest way to
  test sign-up and password-reset flows:

  ```sh
  curl -s -X POST localhost:8025/api/v2/wait -d '{"query":"to:alice@example.com subject:verify","timeout_ms":10000}'
  curl -s localhost:8025/api/v2/messages/<id>/codes      # [{"value":"482913","source":"subject"}]
  curl -N localhost:8025/api/v2/events?since=0          # server-sent events, resumable
  ```

  See [`api/openapi-v2.json`](api/openapi-v2.json); breaking changes are allowed until v1.0.

## Build and run

Requires Rust 1.99.1 (pinned in `rust-toolchain.toml`) and, on Windows, the MSVC build tools.

```sh
cargo build --release
./target/release/spoolbox                        # SMTP on 127.0.0.1:1025, HTTP on 127.0.0.1:8025
./target/release/spoolbox --database mail.db     # keep messages across restarts
./target/release/spoolbox send-sample            # send sample messages (Persian/RTL, attachments, OTP...)
./target/release/spoolbox readyz --wait --timeout 30s
./target/release/spoolbox reindex mail.db        # rebuild the MIME and full-text index
./target/release/spoolbox --help
```

Docker (the image listens on all interfaces inside the container and runs as a non-root user):

```sh
docker build -t spoolbox .
docker run --rm -p 1025:1025 -p 8025:8025 -v spoolbox-data:/data spoolbox --database /data/spoolbox.db
```

## Configuration

Precedence, lowest to highest: built-in defaults → TOML file (`--config` or `SPOOLBOX_CONFIG`) →
`MP_*` variables → `SPOOLBOX_*` variables → command-line flags.

```toml
label = "dev"

[http]
listen = "127.0.0.1:8025"
webroot = "/"
allowed_hosts = []          # extra Host header values; IP literals, localhost and single-label names are always allowed
auth_file = ""              # htpasswd file for the web UI and API (bcrypt, SHA-crypt, APR1/MD5, SHA1, plain)
send_api_auth_file = ""     # separate credentials for POST /api/v1/send (default: auth_file)
send_api_auth_accept_any = false
cors_origins = []           # e.g. ["app.test:3000"] or ["*"]; same-origin requests are always allowed
compression = true          # gzip responses

[smtp]
listen = "127.0.0.1:1025"
max_recipients = 100
max_message_size_mib = 50
max_connections = 1000
tls_cert = ""               # PEM file, or "auto" for a self-signed certificate
tls_key = ""
require_starttls = false
require_tls = false         # implicit TLS (SMTPS)
auth_accept_any = false     # offer AUTH PLAIN/LOGIN and accept any credentials
auth_file = ""              # htpasswd file; AUTH becomes required (and STARTTLS too, unless auth_allow_insecure)
auth_allow_insecure = false # allow PLAIN/LOGIN without TLS
allowed_recipients = ""     # regular expression
ignore_rejected_recipients = false
strict_rfc_headers = false
hide_extensions = []        # e.g. ["PIPELINING", "SMTPUTF8"] to test clients against limited servers
accept_domains = []
reject_domains = []
discard_domains = []

[storage]
database = ""               # empty = temporary database, deleted on exit
max_messages = 500
max_age = ""                # e.g. "12h" or "3d"
min_free_disk_mib = 1024    # refuse mail with 452 below this
compression = 1             # 0 none, 1 fastest, 2 default, 3 best
wal = true
auto_vacuum = true
ignore_duplicate_ids = false

[tags]
disable = []                # "plus-addresses", "x-tags"
title_case = false
username = false

[network]
allow_unauthenticated_network = false

[log]
level = "info"              # error | warn | info | debug | trace
format = "text"             # text | json
file = ""
```

Compatibility flags and `MP_*` variables are recognised. Options that are not implemented
yet are reported at startup; security-relevant ones (authentication, TLS, recipient allowlists)
stop startup instead of being ignored.

Inline credentials can also be given as whitespace-separated `user:password` entries in
`MP_UI_AUTH`, `MP_SEND_API_AUTH` and `MP_SMTP_AUTH`. SMTP authentication needs a
certificate (`--smtp-tls-cert auto` is enough) unless `--smtp-auth-allow-insecure` is set. Chaos testing:
`--enable-chaos` / `--chaos-triggers Sender:451:10,Recipient:452:5,Authentication:535:0`.

**Safe by default:** Spoolbox listens on loopback only. A listener on another interface needs
authentication for that protocol (`--ui-auth-file` for HTTP, `--smtp-auth-file` for SMTP), or an explicit
`--allow-unauthenticated-network` (`SPOOLBOX_ALLOW_UNAUTHENTICATED_NETWORK=true`, e.g. in a container).

## Web UI

The UI lives in [`web/`](web/) (Svelte 5, TypeScript, Vite) and is embedded into the binary at build time;
a binary built without it shows a short page with these instructions instead.

```sh
npm --prefix web ci
npm --prefix web run build      # writes web/dist, picked up by the next cargo build
cargo build --release -p spoolbox
npm --prefix web run dev        # live reload against a server on 127.0.0.1:8025
npm --prefix web run test:e2e   # Playwright: inbox, preview sandbox, XSS corpus, accessibility
```

HTTPS: `--ui-tls-cert auto` (self-signed) or `--ui-tls-cert cert.pem --ui-tls-key key.pem`.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
SPOOLBOX_CRASH_ITERATIONS=1000 cargo test -p spoolbox --test smtp_e2e   # durability under hard kills
python -I tools/phase2/client_compat_smtplib.py target/release/spoolbox  # real-client compatibility
SPOOLBOX_TEST_SMTP_ADDR=127.0.0.1:1025 cargo test -p spoolbox-testkit --test transcripts   # SMTP transcripts against a live server
```

Design documents: [`docs/spec/`](docs/spec/README.md) (Persian) and [`docs/adr/`](docs/adr/README.md).

## Licence

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Goudarz Jafari <hi@goudarzjafari.com>.
Dependency licences: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
