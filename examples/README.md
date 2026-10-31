# Examples

Each example plays both parts: an application that sends a sign-up email, and a test that
waits for it and checks its contents. All of them run in CI against a fresh server.

| Example | Needs | Run |
|---|---|---|
| [Python](python/test_signup.py) | standard library | `python examples/python/test_signup.py` |
| [Node.js](node/signup.test.mjs) | Node 22+, no packages | `node --test examples/node/signup.test.mjs` |
| [Go](go/signup_test.go) | standard library | `cd examples/go && go test ./...` |
| [Shell](shell/signup.sh) | `curl`, `spoolbox wait` | `sh examples/shell/signup.sh` |
| [Rust, in-process](../crates/spoolbox/tests/testkit.rs) | `spoolbox-testkit` with feature `server` | `cargo test -p spoolbox --test testkit` |
| [Docker Compose](docker-compose.yml) | Docker | `docker compose -f examples/docker-compose.yml up` |

They read `SPOOLBOX_URL` (default `http://127.0.0.1:8025/`) and `SPOOLBOX_SMTP` (default
`127.0.0.1:1025`).

## The pattern

1. Take the event cursor: `GET /api/v2/events/cursor` → `{"seq": N}`.
2. Do whatever makes the application send the email.
3. Wait for it: `POST /api/v2/wait` with `{"query": "to:alice@example.com", "since_seq": N}`.
4. Check it: `/api/v2/messages/{id}/codes`, `/links`, `/envelope`, or `/api/v1/message/{id}`.

Taking the cursor first means an email that arrives before the wait starts is still found.

## Pointing a framework at Spoolbox

Spoolbox accepts plain SMTP on port 1025 without authentication by default.

| Framework | Settings |
|---|---|
| Laravel (`.env`) | `MAIL_MAILER=smtp`, `MAIL_HOST=127.0.0.1`, `MAIL_PORT=1025`, `MAIL_ENCRYPTION=null` |
| Symfony (`.env`) | `MAILER_DSN=smtp://127.0.0.1:1025` |
| Django (`settings.py`) | `EMAIL_HOST = "127.0.0.1"`, `EMAIL_PORT = 1025`, `EMAIL_USE_TLS = False` |
| Rails (`config/environments/development.rb`) | `config.action_mailer.delivery_method = :smtp`; `config.action_mailer.smtp_settings = { address: "127.0.0.1", port: 1025 }` |
| Spring Boot (`application.properties`) | `spring.mail.host=127.0.0.1`, `spring.mail.port=1025` |
| ASP.NET / MailKit | `client.Connect("127.0.0.1", 1025, SecureSocketOptions.None)` |
| Nodemailer | `nodemailer.createTransport({ host: "127.0.0.1", port: 1025, secure: false })` |
| Go | `smtp.SendMail("127.0.0.1:1025", nil, from, to, msg)` |

To test TLS or authentication too: `spoolbox --smtp-tls-cert auto --smtp-auth-accept-any`.

## AI assistants (MCP)

`spoolbox mcp` speaks the Model Context Protocol on stdio and forwards to a running server, so an
assistant can read captured mail, wait for a message and pull out codes and links:

```json
{ "mcpServers": { "spoolbox": { "command": "spoolbox", "args": ["mcp"] } } }
```

Clients that use HTTP can connect to `http://127.0.0.1:8025/mcp` directly. Deleting messages is
only offered when the server runs with `--mcp-allow-delete`.
