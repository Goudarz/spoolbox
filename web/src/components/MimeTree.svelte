<script lang="ts">
  import Self from './MimeTree.svelte';
  import { partUrl } from '../lib/api';
  import { formatSize } from '../lib/i18n.svelte';
  import type { MimePart } from '../lib/types';

  let { id, part }: { id: string; part: MimePart } = $props();
  const leaf = $derived(!part.children?.length);
</script>

<li>
  <span class="row">
    {#if leaf}
      <a href={partUrl(id, part.part_id)} target="_blank" rel="noopener noreferrer">{part.part_id}</a>
    {:else}
      <span>{part.part_id}</span>
    {/if}
    <code>{part.content_type}</code>
    {#if part.charset}<span class="muted">{part.charset}</span>{/if}
    {#if part.content_language}<span class="muted" lang={part.content_language}>{part.content_language}</span>{/if}
    {#if part.filename}<span dir="auto">{part.filename}</span>{/if}
    {#if part.disposition}<span class="muted">{part.disposition}</span>{/if}
    {#if part.content_id}<span class="muted">cid:{part.content_id}</span>{/if}
    {#if leaf}<span class="muted">{formatSize(part.size)}</span>{/if}
  </span>
  {#if part.children?.length}
    <ul>
      {#each part.children as child (child.part_id)}
        <Self {id} part={child} />
      {/each}
    </ul>
  {/if}
</li>

<style>
  li {
    margin: 0.15rem 0;
  }
  ul {
    list-style: none;
    padding-inline-start: 1.2rem;
    border-inline-start: 1px dashed var(--border);
    margin: 0.15rem 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    align-items: baseline;
  }
  code {
    font-family: var(--mono);
    font-size: 0.85rem;
  }
</style>
