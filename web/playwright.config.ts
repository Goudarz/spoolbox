import { resolve } from 'node:path';
import { defineConfig } from '@playwright/test';

// The server under test: the debug binary with the UI embedded (run `npm run build` and
// `cargo build -p spoolbox` first). Override with SPOOLBOX_BIN.
const exe = process.platform === 'win32' ? '.exe' : '';
const bin = process.env.SPOOLBOX_BIN ?? resolve(import.meta.dirname, '..', 'target', 'debug', `spoolbox${exe}`);
const http = 28625;
const smtp = 21625;

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://127.0.0.1:${http}/`,
    trace: 'retain-on-failure',
  },
  webServer: {
    command: `"${bin}" --listen 127.0.0.1:${http} --smtp 127.0.0.1:${smtp} --max 0 --log-level warn`,
    url: `http://127.0.0.1:${http}/readyz`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
  metadata: { smtp: `127.0.0.1:${smtp}` },
});
