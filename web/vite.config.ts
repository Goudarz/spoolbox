import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The server injects <base href="{webroot}">, so every URL in the app is relative.
const target = process.env.SPOOLBOX_URL ?? 'http://127.0.0.1:8025';

export default defineConfig({
  base: './',
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    emptyOutDir: true,
    sourcemap: false,
    target: 'es2022',
  },
  server: {
    proxy: {
      '/api/events': { target: target.replace(/^http/, 'ws'), ws: true },
      '/api': target,
      '/view': target,
    },
  },
});
