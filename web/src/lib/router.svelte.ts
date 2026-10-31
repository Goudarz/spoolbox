// Client-side routes, relative to <base href>: "" (inbox), "search?q=…", "m/{id}".

export type Route = { name: 'inbox' } | { name: 'search'; query: string } | { name: 'message'; id: string };

function basePath(): string {
  return new URL(document.baseURI).pathname;
}

function parse(): Route {
  const base = basePath();
  const path = location.pathname.startsWith(base) ? location.pathname.slice(base.length) : '';
  if (path.startsWith('m/')) {
    return { name: 'message', id: decodeURIComponent(path.slice(2)) };
  }
  if (path === 'search') {
    return { name: 'search', query: new URLSearchParams(location.search).get('q') ?? '' };
  }
  return { name: 'inbox' };
}

export const route = $state({ current: parse() });

export function href(r: Route): string {
  switch (r.name) {
    case 'inbox':
      return basePath();
    case 'search':
      return `${basePath()}search?q=${encodeURIComponent(r.query)}`;
    case 'message':
      return `${basePath()}m/${encodeURIComponent(r.id)}`;
  }
}

export function go(r: Route, replace = false) {
  const target = href(r);
  if (replace) history.replaceState(null, '', target);
  else history.pushState(null, '', target);
  route.current = r;
}

window.addEventListener('popstate', () => {
  route.current = parse();
});
