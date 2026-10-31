<script lang="ts">
  import { box, hasMore, loadMore } from '../lib/mailbox.svelte';
  import { go, href } from '../lib/router.svelte';
  import { formatDate, formatSize, t } from '../lib/i18n.svelte';
  import type { Summary } from '../lib/types';

  let { selected = $bindable(0) }: { selected?: number } = $props();

  const sender = (m: Summary) => m.From?.Name || m.From?.Address || '';
  const recipients = (m: Summary) =>
    (m.To ?? []).map((a) => a.Name || a.Address).join(', ');

  let more = $state(false);
  async function next() {
    more = true;
    try {
      await loadMore();
    } finally {
      more = false;
    }
  }

  function open(e: MouseEvent, m: Summary) {
    if (e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    go({ name: 'message', id: m.ID });
  }

  $effect(() => {
    // Keep the keyboard selection visible.
    const row = document.querySelector<HTMLElement>(`[data-index="${selected}"]`);
    row?.scrollIntoView({ block: 'nearest' });
  });
</script>

{#if box.error}
  <p class="state" role="alert">{t('error')}: {box.error}</p>
{:else if !box.loading && box.messages.length === 0}
  <p class="state">{box.query ? t('noResults') : t('noMessages')}</p>
{:else}
  <ul class="list" aria-label={t('inbox')}>
    {#each box.messages as m, i (m.ID)}
      <li class:unread={!m.Read} class:selected={i === selected} data-index={i}>
        <a href={href({ name: 'message', id: m.ID })} onclick={(e) => open(e, m)} onfocus={() => (selected = i)}>
          <span class="from"><bdi>{sender(m)}</bdi></span>
          <span class="when">{formatDate(m.Created)}</span>
          <span class="subject">
            {#if !m.Read}<span class="visually-hidden">{t('unread')}: </span>{/if}<bdi>{m.Subject || t('noSubject')}</bdi>
          </span>
          <span class="meta">
            {#if m.Attachments > 0}<span title={t('attachments')} aria-label={t('attachments')}>📎 {m.Attachments}</span>{/if}
            <span>{formatSize(m.Size)}</span>
          </span>
          <span class="snippet muted">
            <span class="to">{t('to')}: <bdi>{recipients(m)}</bdi></span>
            {#each m.Tags as tag (tag)}<span class="tag"><bdi>{tag}</bdi></span>{/each}
            <bdi class="text">{m.Snippet}</bdi>
          </span>
        </a>
      </li>
    {/each}
  </ul>
  {#if hasMore()}
    <div class="more"><button type="button" onclick={next} disabled={more}>{t('loadMore')}</button></div>
  {/if}
{/if}

<p class="hint muted">{t('shortcuts')}</p>

<style>
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    border-block-end: 1px solid var(--border);
  }
  li.selected {
    background: var(--bg-selected);
  }
  li a {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 0.1rem 1rem;
    padding: 0.55rem 1rem;
    color: inherit;
    text-decoration: none;
  }
  li a:hover {
    background: var(--bg-hover);
  }
  .unread .from,
  .unread .subject {
    font-weight: 650;
  }
  .from,
  .subject,
  .snippet {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .when,
  .meta {
    color: var(--fg-muted);
    font-size: 0.85rem;
    text-align: end;
    white-space: nowrap;
  }
  .meta {
    display: flex;
    gap: 0.6rem;
    justify-content: flex-end;
  }
  .snippet {
    grid-column: 1 / -1;
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
  }
  .to {
    flex: none;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .state,
  .hint {
    padding: 1rem;
    margin: 0;
  }
  .hint {
    font-size: 0.8rem;
  }
  .more {
    padding: 0.8rem 1rem;
  }
  @media (max-width: 40rem) {
    .meta {
      display: none;
    }
  }
</style>
