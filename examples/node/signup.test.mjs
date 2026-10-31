// Testing a sign-up email with Spoolbox from Node.js (node:test and fetch, no packages).
// The application would normally send through a library such as nodemailer; this example
// uses the send API so it has no dependencies.
//
//   node --test examples/node/signup.test.mjs
//
// Set SPOOLBOX_URL if the server does not listen on http://127.0.0.1:8025/.
import { test } from 'node:test';
import assert from 'node:assert/strict';

const base = (process.env.SPOOLBOX_URL ?? 'http://127.0.0.1:8025/').replace(/\/?$/, '/');

async function api(path, body) {
  const res = await fetch(base + path, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${path}: ${res.status} ${await res.text()}`);
  return res.json();
}

test('password reset email contains a working link', async () => {
  const { seq } = await api('api/v2/events/cursor');
  // Stands in for the application under test.
  await api('api/v1/send', {
    From: { Email: 'noreply@acme.test', Name: 'Acme' },
    To: [{ Email: 'bob+node@example.com' }],
    Subject: 'Reset your password',
    HTML: '<p>Click <a href="https://acme.test/reset?token=xyz789">here</a> to reset.</p>',
  });
  const msg = await api('api/v2/wait', { query: 'to:bob+node@example.com subject:reset', since_seq: seq, timeout_ms: 10000 });
  const links = await api(`api/v2/messages/${msg.id}/links`);
  assert.deepEqual(
    links.map((l) => l.url),
    ['https://acme.test/reset?token=xyz789'],
  );
});
