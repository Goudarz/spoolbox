<script lang="ts">
  import Toolbar from './components/Toolbar.svelte';
  import MessageList from './components/MessageList.svelte';
  import MessageView from './components/MessageView.svelte';
  import { api } from './lib/api';
  import { box, load, patch, start, drop } from './lib/mailbox.svelte';
  import { go, route } from './lib/router.svelte';
  import { dir, locale } from './lib/i18n.svelte';

  let selected = $state(0);
  let searchInput: HTMLInputElement | undefined = $state();
  let view: MessageView | undefined = $state();

  $effect(() => start());

  $effect(() => {
    document.documentElement.lang = locale.lang;
    document.documentElement.dir = dir();
  });

  $effect(() => {
    const r = route.current;
    if (r.name === 'inbox') void load('');
    else if (r.name === 'search') void load(r.query);
  });

  $effect(() => {
    document.title = box.unread > 0 ? `(${box.unread}) Spoolbox` : 'Spoolbox';
  });

  function typing(e: KeyboardEvent): boolean {
    const el = e.target as HTMLElement | null;
    return !!el && (el.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName));
  }

  async function onKey(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Escape') {
      if (typing(e)) (e.target as HTMLElement).blur();
      else if (route.current.name === 'message') go({ name: 'inbox' });
      return;
    }
    if (typing(e)) return;
    const inList = route.current.name !== 'message';
    const current = box.messages[selected];
    switch (e.key) {
      case '/':
        e.preventDefault();
        searchInput?.focus();
        break;
      case 'j':
      case 'ArrowDown':
        if (inList) {
          e.preventDefault();
          selected = Math.min(selected + 1, box.messages.length - 1);
        }
        break;
      case 'k':
      case 'ArrowUp':
        if (inList) {
          e.preventDefault();
          selected = Math.max(selected - 1, 0);
        }
        break;
      case 'Enter':
        if (inList && current) go({ name: 'message', id: current.ID });
        break;
      case 'u':
        if (!inList) await view?.markUnread();
        else if (current) {
          await api.setRead([current.ID], !current.Read);
          patch(current.ID, { Read: !current.Read });
        }
        break;
      case 'Delete':
        if (!inList) await view?.remove();
        else if (current) {
          await api.remove([current.ID]);
          drop([current.ID]);
          selected = Math.min(selected, box.messages.length - 1);
        }
        break;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="app">
  <Toolbar bind:searchInput />
  <main>
    {#if route.current.name === 'message'}
      {#key route.current.id}
        <MessageView bind:this={view} id={route.current.id} />
      {/key}
    {:else}
      <MessageList bind:selected />
    {/if}
  </main>
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  main {
    flex: 1;
    overflow: auto;
  }
</style>
