import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';

test('composer shows Kyro tooltips on hover and keyboard focus', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-tooltips-'));
  let app;
  try {
    app = await electron.launch({ args: ['.', `--kyro-profile=${profile}`], env: { ...process.env, KYRO_DESKTOP_DEV: '0' } });
    const page = await app.firstWindow();
    await page.setViewportSize({ width: 1771, height: 1244 });
    const voice = page.getByRole('button', { name: 'Dicter', exact: true });
    const models = page.locator('.team-trigger');
    const settings = page.locator('.composer-preferences > summary');
    const send = page.getByRole('button', { name: 'Envoyer le message' });
    const tooltip = page.getByRole('tooltip');
    await page.getByRole('textbox', { name: 'Votre message' }).fill('Une idée');
    for (const [trigger, label] of [[voice, 'Dicter'], [models, 'Modèles'], [settings, 'Réglages'], [send, 'Envoyer']]) {
      await expect(trigger).not.toHaveAttribute('title');
      await trigger.hover();
      await expect(tooltip).toHaveText(label);
      await page.mouse.move(20, 20);
      await expect(tooltip).toHaveCount(0);
      await trigger.focus();
      await expect(tooltip).toHaveText(label);
      await trigger.press('Escape');
      await expect(tooltip).toHaveCount(0);
      await trigger.blur();
    }
    await voice.hover();
    await expect(tooltip).toHaveText('Dicter');
    await page.screenshot({ path: 'test-results/composer-tooltip.png', animations: 'disabled' });
    await settings.click();
    await expect(tooltip).toHaveCount(0);
    await expect(page.getByRole('region', { name: 'Réglages du message' })).toBeVisible();
    await settings.press('Escape');
    await models.click();
    await expect(tooltip).toHaveCount(0);
    await expect(page.getByRole('region', { name: 'Composition de l’équipe' })).toBeVisible();
    await models.press('Escape');
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await voice.hover();
      await expect(tooltip).toHaveText('Dicter');
      const box = await tooltip.boundingBox();
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(width);
      await page.mouse.move(10, 10);
    }
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await voice.focus();
    await expect(tooltip).toHaveText('Dicter');
    await expect(page.getByRole('textbox', { name: 'Votre message' })).toHaveValue('Une idée');
  } finally {
    await app?.close();
    const target = resolve(profile);
    if (dirname(target) !== resolve(tmpdir()) || !basename(target).startsWith('kyro-tooltips-')) throw new Error('Unexpected test profile path');
    await rm(target, { recursive: true, force: true });
  }
});
