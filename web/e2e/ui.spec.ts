import { expect, test } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { clear, indexed, sendApi, sendRaw } from './helpers';

test.beforeEach(async ({ request }) => {
  await clear(request);
});

test('new mail appears live and opens in a sandboxed preview', async ({ page, request }) => {
  await page.goto('./');
  await expect(page.getByText(/No messages yet/)).toBeVisible();

  const id = await sendApi(request, 'Welcome aboard', '<p>Hello <b>there</b></p>');
  const row = page.getByRole('link', { name: /Welcome aboard/ });
  await expect(row).toBeVisible();

  await row.click();
  await expect(page).toHaveURL(new RegExp(`/m/${id}$`));
  await expect(page.getByRole('heading', { name: 'Welcome aboard' })).toBeVisible();
  const frame = page.locator('iframe');
  await expect(frame).toHaveAttribute('sandbox', 'allow-scripts allow-popups allow-popups-to-escape-sandbox');
  await expect(page.frameLocator('iframe').getByText('there')).toBeVisible();

  await page.getByRole('tab', { name: 'Text' }).click();
  await expect(page.getByRole('tabpanel')).toContainText('text body');
  await page.getByRole('tab', { name: 'Headers' }).click();
  await expect(page.getByRole('tabpanel')).toContainText('Subject');
});

test('search, tags and deleting', async ({ page, request }) => {
  const a = await sendApi(request, 'Invoice 42', undefined, { Tags: ['billing'] });
  await sendApi(request, 'Newsletter');
  await indexed(request, a);
  await page.goto('./');
  await page.getByRole('searchbox').fill('tag:billing');
  await page.getByRole('searchbox').press('Enter');
  await expect(page).toHaveURL(/search\?q=tag%3Abilling/);
  await expect(page.getByRole('link', { name: /Invoice 42/ })).toBeVisible();
  await expect(page.getByRole('link', { name: /Newsletter/ })).toHaveCount(0);

  await page.getByRole('link', { name: /Invoice 42/ }).click();
  await page.getByPlaceholder('Add tag').fill('مهم');
  await page.getByPlaceholder('Add tag').press('Enter');
  await expect(page.locator('.tag', { hasText: 'مهم' })).toBeVisible();

  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole('link', { name: /Invoice 42/ })).toHaveCount(0);
});

test('keyboard navigation', async ({ page, request }) => {
  await sendApi(request, 'First');
  await sendApi(request, 'Second');
  await page.goto('./');
  await expect(page.getByRole('link', { name: /Second/ })).toBeVisible();
  await page.keyboard.press('j');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: 'First' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('link', { name: /First/ })).toBeVisible();
  await page.keyboard.press('/');
  await expect(page.getByRole('searchbox')).toBeFocused();
});

test('remote images can be blocked, with no request leaving the browser', async ({ page, request }) => {
  const external: string[] = [];
  await page.route(/^https?:\/\/(?!127\.0\.0\.1)/, (route) => {
    external.push(route.request().url());
    return route.fulfill({ status: 204 });
  });
  const id = await sendApi(
    request,
    'Tracked',
    '<p>hi</p><img src="https://tracker.invalid/p.gif" width="1" height="1"><img src="https://cdn.invalid/logo.png">',
  );
  await page.goto(`m/${id}`);
  await expect(page.getByText(/1 tracking pixels/)).toBeVisible();

  await page.getByRole('button', { name: 'Block remote images' }).click();
  await expect(page.getByText('Remote images are blocked.')).toBeVisible();
  external.length = 0;
  await page.reload();
  await expect(page.frameLocator('iframe').getByText('hi')).toBeVisible();
  await page.waitForTimeout(500);
  expect(external).toEqual([]);

  await page.getByRole('button', { name: 'Show remote images' }).click();
  await expect.poll(() => external.length).toBeGreaterThan(0);
});

test('persian interface switches to right-to-left', async ({ page, request }) => {
  await sendRaw(
    'From: =?utf-8?b?2YHYsdiz2KrZhtiv2Yc=?= <fa@example.com>\r\nTo: user@example.com\r\nSubject: =?utf-8?b?2LPZgdin2LHYtCDYtNmF2Kc=?=\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nسلام\r\n',
  );
  await page.goto('./');
  await page.getByLabel('Language').selectOption('fa');
  await expect(page.locator('html')).toHaveAttribute('dir', 'rtl');
  await expect(page.getByRole('link', { name: /سفارش شما/ })).toBeVisible();
});

test('inbox and message view have no serious accessibility violations', async ({ page, request }) => {
  const id = await sendApi(request, 'Accessible', '<p>content</p>', {
    Attachments: [{ Content: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', Filename: 'pixel.png' }],
  });
  await indexed(request, id);
  for (const path of ['./', `m/${id}`]) {
    await page.goto(path);
    await expect(page.locator('main')).not.toBeEmpty();
    const result = await new AxeBuilder({ page }).exclude('iframe').analyze();
    const serious = result.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical');
    expect(serious.map((v) => `${v.id}: ${v.help}`), path).toEqual([]);
  }
});

test('the smtp conversation is shown with secrets redacted', async ({ page, request }) => {
  await sendRaw('From: app@example.com\r\nTo: user@example.com\r\nSubject: Transcript check\r\n\r\nbody\r\n', 'app@example.com', ['user@example.com']);
  const list = await (await request.get('api/v1/search?query=subject%3A%22transcript%20check%22')).json();
  await page.goto(`m/${list.messages[0].ID}`);
  await page.getByRole('tab', { name: 'SMTP' }).click();
  const panel = page.getByRole('tabpanel');
  await expect(panel).toContainText('C: MAIL FROM:<app@example.com>');
  await expect(panel).toContainText('# message received');
  await expect(panel).not.toContainText('body');
});
