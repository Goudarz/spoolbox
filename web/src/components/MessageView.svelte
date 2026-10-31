<script lang="ts">
  import { api, ApiError, partUrl, rawUrl, thumbUrl } from '../lib/api';
  import { drop, patch } from '../lib/mailbox.svelte';
  import { go } from '../lib/router.svelte';
  import { formatDate, formatSize, t } from '../lib/i18n.svelte';
  import type { Address, Code, Envelope, Link, Message, MimePart } from '../lib/types';
  import HtmlPreview from './HtmlPreview.svelte';
  import MimeTree from './MimeTree.svelte';
  import TagEditor from './TagEditor.svelte';

  let { id }: { id: string } = $props();

  type Tab = 'html' | 'text' | 'headers' | 'raw' | 'structure' | 'attachments' | 'links' | 'envelope';
  let msg: Message | undefined = $state();
  let error = $state('');
  let tab: Tab = $state('html');
  let headers: Record<string, string[]> | undefined = $state();
  let raw: string | undefined = $state();
  let mime: MimePart | undefined = $state();
  let links: Link[] | undefined = $state();
  let codes: Code[] | undefined = $state();
  let envelope: Envelope | undefined = $state();
  let copied = $state('');

  const parts = $derived(msg ? [...msg.Attachments, ...msg.Inline] : []);
  const pixels = $derived(links?.filter((l) => l.tracking_pixel).length ?? 0);
  const tabs = $derived(
    (['html', 'text', 'headers', 'raw', 'structure', 'attachments', 'links', 'envelope'] as Tab[]).filter(
      (x) => x !== 'attachments' || parts.length > 0,
    ),
  );

  $effect(() => {
    const current = id;
    msg = undefined;
    error = '';
    headers = raw = mime = links = codes = envelope = undefined;
    api
      .message(current)
      .then((m) => {
        if (current !== id) return;
        msg = m;
        tab = m.HTML ? 'html' : 'text';
        patch(current, { Read: true });
        // Links are needed for the tracking-pixel count shown above the preview.
        void api.links(current).then((l) => current === id && (links = l)).catch(() => undefined);
      })
      .catch((e) => {
        error = e instanceof ApiError && e.status === 404 ? t('notFound') : String(e);
      });
  });

  $effect(() => {
    const current = id;
    if (tab === 'headers' && !headers) void api.headers(current).then((h) => (headers = h));
    if (tab === 'raw' && raw === undefined) void api.raw(current).then((r) => (raw = r));
    if (tab === 'structure' && !mime) void api.mime(current).then((m) => (mime = m)).catch(() => undefined);
    if (tab === 'links' && !codes) void api.codes(current).then((c) => (codes = c));
    if (tab === 'envelope' && !envelope) void api.envelope(current).then((e) => (envelope = e));
  });

  const label: Record<Tab, () => string> = {
    html: () => t('html'),
    text: () => t('text'),
    headers: () => t('headers'),
    raw: () => t('raw'),
    structure: () => t('structure'),
    attachments: () => `${t('attachments')} (${parts.length})`,
    links: () => t('links'),
    envelope: () => t('envelope'),
  };

  const show = (list: Address[]) => list.map((a) => (a.Name ? `${a.Name} <${a.Address}>` : a.Address)).join(', ');

  function tabKey(e: KeyboardEvent) {
    const i = tabs.indexOf(tab);
    const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const rtl = getComputedStyle(e.currentTarget as Element).direction === 'rtl';
    const next = tabs[(i + (rtl ? -step : step) + tabs.length) % tabs.length];
    if (next) {
      tab = next;
      document.getElementById(`tab-${next}`)?.focus();
    }
  }

  export async function markUnread() {
    if (!msg) return;
    await api.setRead([msg.ID], false);
    patch(msg.ID, { Read: false });
  }

  export async function remove() {
    if (!msg) return;
    await api.remove([msg.ID]);
    drop([msg.ID]);
    go({ name: 'inbox' });
  }

  async function copy(value: string) {
    await navigator.clipboard.writeText(value);
    copied = value;
    setTimeout(() => (copied = ''), 1500);
  }
</script>

<article class="view">
  <div class="top">
    <button type="button" onclick={() => history.length > 1 ? history.back() : go({ name: 'inbox' })}>{t('back')}</button>
    {#if msg}
      <button type="button" onclick={markUnread}>{t('markUnread')}</button>
      <a class="button" href={rawUrl(msg.ID)} download>{t('download')}</a>
      <button type="button" class="danger" onclick={remove}>{t('delete')}</button>
    {/if}
  </div>

  {#if error}
    <p role="alert">{error}</p>
  {:else if !msg}
    <p class="muted" aria-busy="true">…</p>
  {:else}
    <h1 dir="auto">{msg.Subject || t('noSubject')}</h1>
    <dl class="head">
      {#if msg.From}<dt>{t('from')}</dt><dd dir="auto">{show([msg.From])}</dd>{/if}
      {#if msg.To.length}<dt>{t('to')}</dt><dd dir="auto">{show(msg.To)}</dd>{/if}
      {#if msg.Cc.length}<dt>{t('cc')}</dt><dd dir="auto">{show(msg.Cc)}</dd>{/if}
      {#if msg.Bcc.length}<dt>{t('bcc')}</dt><dd dir="auto">{show(msg.Bcc)}</dd>{/if}
      {#if msg.ReplyTo.length}<dt>{t('replyTo')}</dt><dd dir="auto">{show(msg.ReplyTo)}</dd>{/if}
      <dt>{t('date')}</dt><dd>{formatDate(msg.Date)} · {formatSize(msg.Size)}</dd>
      <dt>{t('tags')}</dt><dd><TagEditor id={msg.ID} bind:tags={msg.Tags} /></dd>
    </dl>

    <div class="tabs" role="tablist" tabindex="-1" onkeydown={tabKey}>
      {#each tabs as x (x)}
        <button
          type="button"
          role="tab"
          id="tab-{x}"
          aria-selected={tab === x}
          aria-controls="panel"
          tabindex={tab === x ? 0 : -1}
          onclick={() => (tab = x)}>{label[x]()}</button
        >
      {/each}
    </div>

    <div id="panel" role="tabpanel" aria-labelledby="tab-{tab}" class="panel">
      {#if tab === 'html'}
        {#if msg.HTML}
          <HtmlPreview id={msg.ID} trackingPixels={pixels} />
        {:else}
          <p class="muted">{t('noHtml')}</p>
        {/if}
      {:else if tab === 'text'}
        <pre dir="auto">{msg.Text}</pre>
      {:else if tab === 'headers'}
        {#if headers}
          <table>
            <tbody>
              {#each Object.entries(headers) as [name, values] (name)}
                {#each values as v, i (i)}
                  <tr><th scope="row">{name}</th><td><pre dir="auto">{v}</pre></td></tr>
                {/each}
              {/each}
            </tbody>
          </table>
        {/if}
      {:else if tab === 'raw'}
        {#if raw !== undefined}<pre dir="ltr">{raw}</pre>{/if}
      {:else if tab === 'structure'}
        {#if mime}<ul class="tree"><MimeTree id={msg.ID} part={mime} /></ul>{/if}
      {:else if tab === 'attachments'}
        <ul class="files">
          {#each parts as a (a.PartID)}
            <li>
              <a href={partUrl(msg.ID, a.PartID)} target="_blank" rel="noopener noreferrer">
                {#if a.ContentType.startsWith('image/')}
                  <img src={thumbUrl(msg.ID, a.PartID)} alt="" width="180" height="120" loading="lazy" />
                {/if}
                <span dir="auto">{a.FileName || a.ContentID || a.PartID}</span>
              </a>
              <span class="muted">{a.ContentType} · {formatSize(a.Size)}</span>
            </li>
          {/each}
        </ul>
      {:else if tab === 'links'}
        {#if codes?.length}
          <h2>{t('codes')}</h2>
          <ul class="codes">
            {#each codes as c (c.value)}
              <li><code>{c.value}</code> <span class="muted">{c.source}</span>
                <button type="button" onclick={() => copy(c.value)}>{copied === c.value ? t('copied') : t('copy')}</button></li>
            {/each}
          </ul>
        {/if}
        {#if links?.length}
          <ul class="links">
            {#each links as l (l.url)}
              <li>
                <a href={l.url} target="_blank" rel="noopener noreferrer" dir="ltr">{l.url}</a>
                <span class="muted">{l.source}{l.tracking_pixel ? ` · ${t('trackingPixels')}` : ''}</span>
                {#if l.text}<span dir="auto">“{l.text}”</span>{/if}
              </li>
            {/each}
          </ul>
        {:else if links}
          <p class="muted">{t('noLinks')}</p>
        {/if}
      {:else if tab === 'envelope'}
        {#if envelope}
          <dl class="head">
            <dt>{t('envelopeFrom')}</dt><dd>{envelope.mail_from || '<>'}</dd>
            <dt>{t('envelopeTo')}</dt><dd>{envelope.rcpt_to.join(', ')}</dd>
            <dt>{t('client')}</dt><dd>{envelope.helo} {envelope.remote_ip} {envelope.tls ?? ''} {envelope.auth_user ?? ''}</dd>
          </dl>
        {/if}
      {/if}
    </div>
  {/if}
</article>

<style>
  .view {
    padding: 0.8rem 1rem 2rem;
    max-width: 72rem;
  }
  .top {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .button {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.3rem 0.7rem;
    text-decoration: none;
    color: inherit;
  }
  h1 {
    font-size: 1.3rem;
    margin: 0.9rem 0 0.4rem;
    overflow-wrap: anywhere;
  }
  h2 {
    font-size: 1rem;
  }
  .head {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 0.2rem 0.8rem;
    margin: 0 0 0.8rem;
  }
  .head dt {
    color: var(--fg-muted);
  }
  .head dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    border-block-end: 1px solid var(--border);
  }
  .tabs button {
    border: 0;
    border-block-end: 2px solid transparent;
    border-radius: 0;
    background: none;
  }
  .tabs button[aria-selected='true'] {
    border-block-end-color: var(--accent);
    color: var(--accent);
  }
  .panel {
    padding-block-start: 0.6rem;
  }
  table {
    border-collapse: collapse;
    width: 100%;
  }
  th {
    text-align: start;
    vertical-align: top;
    padding: 0.2rem 0.8rem 0.2rem 0;
    white-space: nowrap;
    font-weight: 500;
    color: var(--fg-muted);
  }
  td {
    padding: 0.2rem 0;
  }
  .tree {
    list-style: none;
    padding: 0;
  }
  .files {
    list-style: none;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12rem, 1fr));
    gap: 0.8rem;
  }
  .files li {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  .files a {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    overflow-wrap: anywhere;
  }
  .files img {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    object-fit: cover;
    max-width: 100%;
    height: auto;
  }
  .links,
  .codes {
    padding-inline-start: 1.1rem;
  }
  .links li,
  .codes li {
    margin: 0.3rem 0;
    overflow-wrap: anywhere;
  }
</style>
