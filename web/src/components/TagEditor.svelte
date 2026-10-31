<script lang="ts">
  import { api } from '../lib/api';
  import { patch } from '../lib/mailbox.svelte';
  import { t } from '../lib/i18n.svelte';

  let { id, tags = $bindable([]) }: { id: string; tags?: string[] } = $props();
  let text = $state('');
  let busy = $state(false);

  async function save(next: string[]) {
    busy = true;
    try {
      await api.setTags(id, next);
      tags = next;
      patch(id, { Tags: next });
    } finally {
      busy = false;
    }
  }

  function add(e: SubmitEvent) {
    e.preventDefault();
    const tag = text.trim();
    if (tag && !tags.some((x) => x.toLowerCase() === tag.toLowerCase())) void save([...tags, tag]);
    text = '';
  }
</script>

<div class="tags">
  {#each tags as tag (tag)}
    <span class="tag" dir="auto">
      {tag}
      <button type="button" class="remove" aria-label="{t('delete')} {tag}" disabled={busy} onclick={() => save(tags.filter((x) => x !== tag))}>×</button>
    </span>
  {/each}
  <form onsubmit={add}>
    <label class="visually-hidden" for="tag-{id}">{t('addTag')}</label>
    <input id="tag-{id}" dir="auto" placeholder={t('addTag')} bind:value={text} disabled={busy} />
  </form>
</div>

<style>
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
    align-items: center;
  }
  .remove {
    border: 0;
    background: none;
    padding: 0 0 0 0.2rem;
    color: inherit;
    line-height: 1;
  }
  input {
    width: 9rem;
    padding: 0.1rem 0.45rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    font-size: 0.8rem;
  }
</style>
