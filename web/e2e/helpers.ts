import { createConnection } from 'node:net';
import { readFileSync } from 'node:fs';
import type { APIRequestContext } from '@playwright/test';

export const SMTP = { host: '127.0.0.1', port: 21625 };

/** Minimal SMTP client: sends one raw message (CRLF line endings, dot-stuffed). */
export function sendRaw(raw: string, from = 'sender@example.com', to = ['victim@example.com']): Promise<void> {
  const body = raw.replace(/\r?\n/g, '\r\n').replace(/^\./gm, '..');
  const script = [
    'EHLO e2e.test',
    `MAIL FROM:<${from}>`,
    ...to.map((r) => `RCPT TO:<${r}>`),
    'DATA',
    `${body}${body.endsWith('\r\n') ? '' : '\r\n'}.`,
    'QUIT',
  ];
  return new Promise((resolve, reject) => {
    const sock = createConnection(SMTP);
    let buffer = '';
    let step = -1;
    sock.setEncoding('utf8');
    sock.on('data', (chunk: string) => {
      buffer += chunk;
      // Wait for the last line of a (possibly multi-line) reply.
      const lines = buffer.split('\r\n').filter(Boolean);
      const last = lines[lines.length - 1];
      if (!last || last[3] === '-' || !buffer.endsWith('\r\n')) return;
      buffer = '';
      const code = Number(last.slice(0, 3));
      if (code >= 400) {
        sock.destroy();
        reject(new Error(`SMTP error after ${script[step] ?? 'connect'}: ${last}`));
        return;
      }
      step++;
      if (step < script.length) sock.write(`${script[step]}\r\n`);
      else sock.end();
    });
    sock.on('close', () => resolve());
    sock.on('error', reject);
  });
}

export const sendFile = (path: string) => sendRaw(readFileSync(path, 'utf8'));

export async function sendApi(request: APIRequestContext, subject: string, html?: string, extra: object = {}) {
  const res = await request.post('api/v1/send', {
    data: { From: { Email: 'app@example.com', Name: 'App' }, To: [{ Email: 'user@example.com' }], Subject: subject, Text: 'text body', HTML: html, ...extra },
  });
  if (!res.ok()) throw new Error(`send failed: ${res.status()} ${await res.text()}`);
  return ((await res.json()) as { ID: string }).ID;
}

export async function clear(request: APIRequestContext) {
  await request.delete('api/v1/messages', { data: {} });
}

/** Waits until the background indexer has processed a message (attachments, snippet, search). */
export async function indexed(request: APIRequestContext, id: string) {
  for (let i = 0; i < 100; i++) {
    const res = await request.get(`api/v2/messages/${id}/mime`);
    if (res.ok()) return;
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error(`message ${id} was not indexed`);
}
