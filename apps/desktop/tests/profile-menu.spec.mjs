import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';

test('profile menu preserves local state and supports keyboard and compact layouts', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-profile-menu-'));
  let app;
  try {
    app = await electron.launch({ args: ['.', `--kyro-profile=${profile}`], env: { ...process.env, KYRO_DESKTOP_DEV: '0' } });
    const page = await app.firstWindow();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.setViewportSize({ width: 1771, height: 1244 });
    const trigger = page.locator('.profile > summary');
    const menu = page.getByRole('region', { name: 'Menu du profil' });
    const draft = page.getByRole('textbox', { name: 'Votre message' });
    await draft.fill('Mon brouillon');
    await trigger.click();
    await expect(menu).toBeVisible();
    expect(await menu.locator('button').allTextContents()).toEqual(['Utilisation', 'Inviter un ami', 'Paramètres', 'Se déconnecter']);
    await expect(menu.getByRole('button', { name: 'Se déconnecter' })).toBeDisabled();
    await expect(menu.locator('hr')).toHaveCount(1);
    await trigger.press('ArrowDown');
    await expect(menu.getByRole('button', { name: /^Utilisation/ })).toBeFocused();
    await page.keyboard.press('End');
    await expect(menu.getByRole('button', { name: 'Paramètres', exact: true })).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(menu).not.toBeVisible();
    await expect(trigger).toBeFocused();
    for (const name of ['Utilisation', 'Inviter un ami', 'Paramètres']) {
      await trigger.click();
      await menu.getByRole('button', { name: new RegExp(`^${name}`) }).click();
      await expect(page.getByRole('dialog', { name, exact: true })).toBeVisible();
      await expect(menu).not.toBeVisible();
      await page.getByRole('button', { name: 'Compris', exact: true }).click();
      await expect(trigger).toBeFocused();
      await expect(draft).toHaveValue('Mon brouillon');
    }
    for (const width of [1771, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await trigger.click();
      const box = await menu.boundingBox();
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(width);
      expect(box.y).toBeGreaterThanOrEqual(0);
      expect(box.y + box.height).toBeLessThanOrEqual(900);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.getByRole('heading', { name: 'Kyro', exact: true }).click();
      await expect(menu).not.toBeVisible();
    }
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await trigger.click();
    await expect(menu).toBeVisible();
    await page.screenshot({ path: 'test-results/profile-dark.png', animations: 'disabled' });
    expect(errors).toEqual([]);
  } finally {
    await app?.close();
    const target = resolve(profile);
    if (dirname(target) !== resolve(tmpdir()) || !basename(target).startsWith('kyro-profile-menu-')) throw new Error('Unexpected test profile path');
    await rm(target, { recursive: true, force: true });
  }
});
