import { readdirSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '@playwright/test';
import { clear, sendFile } from './helpers';

const corpus = join(import.meta.dirname, '..', '..', 'fixtures', 'xss');
const files = readdirSync(corpus).filter((f) => f.endsWith('.eml'));

test.beforeAll(async ({ request }) => {
  await clear(request);
  for (const f of files) await sendFile(join(corpus, f));
});

for (const f of files) {
  const name = f.replace(/\.eml$/, '');
  test(`no payload runs: ${name}`, async ({ page }) => {
    const dialogs: string[] = [];
    page.on('dialog', async (d) => {
      dialogs.push(d.message());
      await d.dismiss();
    });
    await page.goto('./');
    await page.getByRole('link', { name: new RegExp(`xss ${name}`) }).click();
    const frame = page.frameLocator('iframe');
    await expect(frame.locator('body')).toBeAttached();
    // Give payloads time to fire, then try the obvious interaction.
    await page.waitForTimeout(400);
    const link = frame.locator('a').first();
    if (await link.count()) await link.click({ timeout: 1000 }).catch(() => undefined);
    await page.waitForTimeout(200);

    expect(dialogs).toEqual([]);
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss)).toBeUndefined();
    // The preview runs in an opaque origin: it cannot read the UI's document.
    const crossed = await page
      .frames()[1]
      ?.evaluate(() => {
        try {
          return window.parent.document.title.length > 0;
        } catch {
          return false;
        }
      })
      .catch(() => false);
    expect(crossed ?? false).toBe(false);
  });
}
