// Inbox state shared by the list and the message view, kept current by live notifications.
import { api, connectEvents } from './api';
import type { Notification, Summary, WebUi } from './types';

const PAGE = 50;

export const box = $state({
  messages: [] as Summary[],
  query: '',
  /** Messages matching the current list (all, or the search). */
  matched: 0,
  total: 0,
  unread: 0,
  tags: [] as string[],
  loading: false,
  error: '',
  connected: false,
  ui: { Label: '', ChaosEnabled: false, DuplicatesIgnored: false, HideDeleteAllButton: false } as WebUi,
});

let generation = 0;

export async function load(query = box.query) {
  const gen = ++generation;
  box.query = query;
  box.loading = true;
  box.error = '';
  try {
    const page = query ? await api.search(query, 0, PAGE) : await api.list(0, PAGE);
    if (gen !== generation) return;
    box.messages = page.messages;
    box.matched = page.messages_count;
    box.total = page.total;
    box.unread = page.unread;
    box.tags = page.tags;
  } catch (e) {
    if (gen === generation) box.error = e instanceof Error ? e.message : String(e);
  } finally {
    if (gen === generation) box.loading = false;
  }
}

export async function loadMore() {
  const gen = generation;
  const start = box.messages.length;
  const page = box.query ? await api.search(box.query, start, PAGE) : await api.list(start, PAGE);
  if (gen !== generation) return;
  const seen = new Set(box.messages.map((m) => m.ID));
  box.messages = [...box.messages, ...page.messages.filter((m) => !seen.has(m.ID))];
  box.matched = page.messages_count;
}

export const hasMore = () => box.messages.length < box.matched;

/** Local update after an action, so the UI reacts before the notification arrives. */
export function patch(id: string, change: Partial<Summary>) {
  const m = box.messages.find((x) => x.ID === id);
  if (m) {
    if (change.Read !== undefined && change.Read !== m.Read) box.unread += change.Read ? -1 : 1;
    Object.assign(m, change);
  }
}

export function drop(ids: string[]) {
  const gone = new Set(ids);
  const before = box.messages.length;
  box.messages = box.messages.filter((m) => !gone.has(m.ID));
  box.matched = Math.max(0, box.matched - (before - box.messages.length));
}

let reloadTimer: ReturnType<typeof setTimeout> | undefined;
function reloadSoon() {
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(() => void load(), 200);
}

function onNotice(n: Notification) {
  switch (n.Type) {
    case 'new':
      // A search only shows new mail after a reload, because matching happens on the server.
      if (box.query) {
        reloadSoon();
      } else if (!box.messages.some((m) => m.ID === n.Data.ID)) {
        box.messages = [n.Data, ...box.messages];
        box.matched++;
      }
      break;
    case 'update': {
      const m = box.messages.find((x) => x.ID === n.Data.ID);
      if (m && n.Data.Read !== undefined) m.Read = n.Data.Read;
      if (m && n.Data.Tags !== undefined) m.Tags = n.Data.Tags;
      break;
    }
    case 'delete':
      drop([n.Data.ID]);
      break;
    case 'truncate':
      box.messages = [];
      box.matched = 0;
      break;
    case 'prune':
      reloadSoon();
      break;
    case 'stats':
      box.total = n.Data.Total;
      box.unread = n.Data.Unread;
      break;
  }
}

export function start(): () => void {
  void api
    .webui()
    .then((ui) => (box.ui = ui))
    .catch(() => undefined);
  return connectEvents(onNotice, (c) => {
    // After a reconnect the list may have missed changes.
    if (c && !box.connected && box.messages.length) reloadSoon();
    box.connected = c;
  });
}
