<script lang="ts">
  import { viewUrl } from '../lib/api';
  import { t } from '../lib/i18n.svelte';

  let { id, trackingPixels = 0 }: { id: string; trackingPixels?: number } = $props();

  function initialRemote(): boolean {
    try {
      return localStorage.getItem('spoolbox.remote') !== 'block';
    } catch {
      return true;
    }
  }

  let remote = $state(initialRemote());
  let height = $state(400);
  let frame: HTMLIFrameElement | undefined = $state();

  function toggle() {
    remote = !remote;
    try {
      localStorage.setItem('spoolbox.remote', remote ? 'show' : 'block');
    } catch {
      // Not persisted.
    }
  }

  $effect(() => {
    // Only height reports from our own frame are accepted; the frame has an opaque origin.
    const onMessage = (e: MessageEvent) => {
      if (!frame || e.source !== frame.contentWindow) return;
      const h = (e.data as { messageHeight?: unknown } | null)?.messageHeight;
      if (typeof h === 'number' && Number.isFinite(h)) height = Math.min(Math.max(h + 24, 120), 20000);
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  });
</script>

<div class="bar" role="status">
  <span>{remote ? t('remoteLoaded') : t('remoteBlocked')}</span>
  {#if trackingPixels > 0}<span class="pixels">· {trackingPixels} {t('trackingPixels')}</span>{/if}
  <button type="button" onclick={toggle}>{remote ? t('blockRemote') : t('showRemote')}</button>
</div>
<!-- No allow-same-origin: email HTML can never reach the UI, its cookies or the API. -->
<iframe
  bind:this={frame}
  title={t('html')}
  src={viewUrl(id, remote)}
  sandbox="allow-scripts allow-popups allow-popups-to-escape-sandbox"
  referrerpolicy="no-referrer"
  style:height="{height}px"
></iframe>

<style>
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0;
    color: var(--fg-muted);
    font-size: 0.85rem;
  }
  .pixels {
    color: var(--danger);
  }
  iframe {
    display: block;
    width: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: #fff;
  }
</style>
