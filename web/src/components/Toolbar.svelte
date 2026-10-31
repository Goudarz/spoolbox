<script lang="ts">
  import { api } from '../lib/api';
  import { box, load } from '../lib/mailbox.svelte';
  import { go, route } from '../lib/router.svelte';
  import { languages, locale, setLang, t, type Lang } from '../lib/i18n.svelte';

  let { searchInput = $bindable() }: { searchInput?: HTMLInputElement } = $props();

  let text = $state(route.current.name === 'search' ? route.current.query : '');

  $effect(() => {
    if (route.current.name === 'search') text = route.current.query;
    else if (route.current.name === 'inbox') text = '';
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    const query = text.trim();
    go(query ? { name: 'search', query } : { name: 'inbox' });
  }

  function clear() {
    text = '';
    go({ name: 'inbox' });
  }

  async function markAllRead() {
    await api.markAllRead();
    await load();
  }

  async function deleteAll() {
    if (box.query) {
      if (!confirm(t('confirmDeleteResults'))) return;
      await api.removeMatching(box.query);
    } else {
      if (!confirm(t('confirmDeleteAll'))) return;
      await api.removeAll();
    }
    await load();
  }
</script>

<header class="toolbar">
  <a class="brand" href={new URL('.', document.baseURI).pathname} onclick={(e) => { e.preventDefault(); go({ name: 'inbox' }); }}>
    <img src="favicon.svg" alt="" width="22" height="22" />
    <span>Spoolbox</span>
    {#if box.ui.Label}<span class="label" dir="auto">{box.ui.Label}</span>{/if}
  </a>

  <form class="search" role="search" onsubmit={submit}>
    <label class="visually-hidden" for="search">{t('search')}</label>
    <input
      id="search"
      type="search"
      dir="auto"
      autocomplete="off"
      spellcheck="false"
      placeholder={t('searchPlaceholder')}
      bind:value={text}
      bind:this={searchInput}
    />
    {#if text}<button type="button" onclick={clear}>{t('clear')}</button>{/if}
  </form>

  <div class="actions">
    <span class="count" aria-live="polite">
      {box.unread} {t('unread')} · {box.total} {t('messages')}
    </span>
    <span class="status" class:on={box.connected} title={box.connected ? t('live') : t('offline')}>
      <span class="visually-hidden">{box.connected ? t('live') : t('offline')}</span>
    </span>
    {#if route.current.name !== 'message'}
    <button type="button" onclick={markAllRead} disabled={box.unread === 0}>{t('markAllRead')}</button>
    {/if}
    {#if !box.ui.HideDeleteAllButton && route.current.name !== 'message'}
      <button type="button" class="danger" onclick={deleteAll} disabled={box.messages.length === 0}>
        {box.query ? t('deleteResults') : t('deleteAll')}
      </button>
    {/if}
    <label class="visually-hidden" for="lang">{t('language')}</label>
    <select id="lang" value={locale.lang} onchange={(e) => setLang(e.currentTarget.value as Lang)}>
      {#each Object.entries(languages) as [code, name] (code)}
        <option value={code}>{name}</option>
      {/each}
    </select>
  </div>
</header>

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem 1rem;
    padding: 0.55rem 1rem;
    border-block-end: 1px solid var(--border);
    background: var(--bg-soft);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-weight: 600;
    color: var(--fg);
    text-decoration: none;
  }
  .label {
    font-weight: 400;
    color: var(--fg-muted);
  }
  .search {
    display: flex;
    flex: 1 1 20rem;
    gap: 0.4rem;
  }
  .search input {
    flex: 1;
    min-width: 0;
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .count {
    color: var(--fg-muted);
    white-space: nowrap;
  }
  .status {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: var(--danger);
  }
  .status.on {
    background: var(--ok);
  }
  select {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    padding: 0.25rem 0.4rem;
  }
</style>
