// API client. All URLs are relative to <base href> (the server's --webroot).
import type {
  Code,
  Envelope,
  Link,
  Message,
  MessagesPage,
  MimePart,
  Transcript,
  Notification,
  WebUi,
} from './types';

export const url = (path: string): string => new URL(path, document.baseURI).toString();

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, headers: {} };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    (init.headers as Record<string, string>)['Content-Type'] = 'application/json';
  }
  const res = await fetch(url(path), init);
  const text = await res.text();
  if (!res.ok) {
    throw new ApiError(res.status, text.trim() || res.statusText);
  }
  const type = res.headers.get('Content-Type') ?? '';
  return (type.includes('json') ? JSON.parse(text) : text) as T;
}

const q = (params: Record<string, string | number | undefined>): string => {
  const s = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== '') s.set(k, String(v));
  }
  const out = s.toString();
  return out ? `?${out}` : '';
};

export const api = {
  list: (start: number, limit: number) =>
    call<MessagesPage>('GET', `api/v1/messages${q({ start, limit })}`),
  search: (query: string, start: number, limit: number) =>
    call<MessagesPage>('GET', `api/v1/search${q({ query, start, limit, tz: timeZone() })}`),
  message: (id: string) => call<Message>('GET', `api/v1/message/${encodeURIComponent(id)}`),
  headers: (id: string) =>
    call<Record<string, string[]>>('GET', `api/v1/message/${encodeURIComponent(id)}/headers`),
  raw: (id: string) => call<string>('GET', `api/v1/message/${encodeURIComponent(id)}/raw`),
  setRead: (ids: string[], read: boolean) => call<string>('PUT', 'api/v1/messages', { IDs: ids, Read: read }),
  markAllRead: () => call<string>('PUT', 'api/v1/messages', { Read: true }),
  remove: (ids: string[]) => call<string>('DELETE', 'api/v1/messages', { IDs: ids }),
  removeAll: () => call<string>('DELETE', 'api/v1/messages', {}),
  removeMatching: (query: string) => call<string>('DELETE', `api/v1/search${q({ query, tz: timeZone() })}`),
  setTags: (id: string, tags: string[]) => call<string>('PUT', 'api/v1/tags', { IDs: [id], Tags: tags }),
  tags: () => call<string[]>('GET', 'api/v1/tags'),
  webui: () => call<WebUi>('GET', 'api/v1/webui'),
  envelope: (id: string) => call<Envelope>('GET', `api/v2/messages/${encodeURIComponent(id)}/envelope`),
  mime: (id: string) => call<MimePart>('GET', `api/v2/messages/${encodeURIComponent(id)}/mime`),
  links: (id: string) => call<Link[]>('GET', `api/v2/messages/${encodeURIComponent(id)}/links`),
  codes: (id: string) => call<Code[]>('GET', `api/v2/messages/${encodeURIComponent(id)}/codes`),
  transcript: (id: string) =>
    call<Transcript>('GET', `api/v2/messages/${encodeURIComponent(id)}/transcript`),
};

export const partUrl = (id: string, part: string) =>
  url(`api/v1/message/${encodeURIComponent(id)}/part/${encodeURIComponent(part)}`);
export const thumbUrl = (id: string, part: string) => `${partUrl(id, part)}/thumb`;
export const rawUrl = (id: string) => url(`api/v1/message/${encodeURIComponent(id)}/raw?dl=1`);
export const viewUrl = (id: string, remote: boolean) =>
  url(`view/${encodeURIComponent(id)}.html?embed=1${remote ? '' : '&remote=0'}`);

function timeZone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
}

/** Live notifications; reconnects with backoff and reports the connection state. */
export function connectEvents(
  onNotice: (n: Notification) => void,
  onState: (connected: boolean) => void,
): () => void {
  let socket: WebSocket | undefined;
  let retry = 500;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const open = () => {
    const u = new URL('api/events', document.baseURI);
    u.protocol = u.protocol === 'https:' ? 'wss:' : 'ws:';
    socket = new WebSocket(u);
    socket.onopen = () => {
      retry = 500;
      onState(true);
    };
    socket.onmessage = (e) => {
      try {
        onNotice(JSON.parse(String(e.data)) as Notification);
      } catch {
        // Ignore malformed frames.
      }
    };
    socket.onclose = () => {
      onState(false);
      if (!stopped) {
        timer = setTimeout(open, retry);
        retry = Math.min(retry * 2, 10_000);
      }
    };
  };
  open();
  return () => {
    stopped = true;
    clearTimeout(timer);
    socket?.close();
  };
}
