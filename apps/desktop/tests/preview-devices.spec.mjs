import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test('preview formats resize the canvas and preserve content and draft', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-preview-'));
  let app;
  try {
    app = await electron.launch({ args: ['.', `--kyro-profile=${profile}`], env: { ...process.env, KYRO_DESKTOP_DEV: '0' } });
    const page = await app.firstWindow();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width: 1871, height: 1100 });
    const formats = page.getByRole('group', { name: 'Format de l’aperçu' });
    const canvas = page.locator('.preview-canvas');
    const draft = page.getByRole('textbox', { name: 'Votre message' });
    await expect(formats.getByRole('button', { name: 'Ordinateur', exact: true })).toHaveAttribute('aria-pressed', 'true');
    await draft.fill('Mon application responsive');
    const desktopWidth = (await canvas.boundingBox()).width;
    await formats.getByRole('button', { name: 'Tablette', exact: true }).click();
    await expect(canvas).toHaveAttribute('data-device', 'tablet');
    expect((await canvas.boundingBox()).width).toBe(768);
    const phone = formats.getByRole('button', { name: 'iPhone', exact: true });
    await phone.focus();
    await phone.press('Enter');
    await expect(phone).toHaveAttribute('aria-pressed', 'true');
    expect((await canvas.boundingBox()).width).toBe(393);
    await expect(draft).toHaveValue('Mon application responsive');
    await page.getByRole('button', { name: 'Pages', exact: true }).click();
    await expect(canvas).toHaveAttribute('data-device', 'phone');
    await expect(canvas.getByRole('heading', { name: 'Pages', exact: true })).toBeVisible();
    const desktop = formats.getByRole('button', { name: 'Ordinateur', exact: true });
    await desktop.focus();
    await desktop.press('Space');
    expect((await canvas.boundingBox()).width).toBe(desktopWidth);
    await formats.getByRole('button', { name: 'Tablette', exact: true }).click();
    await page.screenshot({ path: 'test-results/preview-tablet.png' });
    await phone.click();
    await page.screenshot({ path: 'test-results/preview-iphone.png' });
    for (const width of [1000, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await expect(formats).toBeVisible();
      for (const name of ['Ordinateur', 'Tablette', 'iPhone']) {
        await formats.getByRole('button', { name, exact: true }).click();
        const box = await canvas.boundingBox();
        expect(box.x).toBeGreaterThanOrEqual(0);
        expect(box.x + box.width).toBeLessThanOrEqual(width);
        const group = await formats.boundingBox();
        expect(group.x + group.width).toBeLessThanOrEqual(width);
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      }
    }
    expect(errors).toEqual([]);
  } finally {
    await app?.close();
    await rm(profile, { recursive: true, force: true });
  }
});
