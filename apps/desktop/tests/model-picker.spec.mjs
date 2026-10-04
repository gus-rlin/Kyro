import { test, expect, _electron as electron } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname, basename } from 'node:path';

test('Nano menus restrict models and tools, preserve context and support keyboard and compact layouts', async () => {
  const profile = await mkdtemp(join(tmpdir(), 'kyro-model-picker-'));
  let app;
  try {
    app = await electron.launch({ args: ['.', `--kyro-profile=${profile}`], env: { ...process.env, KYRO_DESKTOP_DEV: '0' } });
    const page = await app.firstWindow();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const picker = page.locator('.team-trigger');
    const panel = page.getByRole('region', { name: 'Composition de l’équipe' });
    const model = page.getByLabel('Modèle : Nemotron 3 Nano', { exact: true });
    const choices = page.getByRole('group', { name: 'Choix modèle', exact: true });
    const draft = page.getByRole('textbox', { name: 'Votre message' });
    const settings = page.locator('.composer-preferences > summary');
    const settingsPanel = page.getByRole('region', { name: 'Réglages du message' });
    await draft.fill('Une idée <script>test</script>');
    await expect(picker).toContainText('Nano');
    await picker.focus();
    await picker.press('Enter');
    await expect(panel).toContainText('Sous-agents indisponibles');
    await expect(panel.getByRole('button', { name: 'Plus de sous-agents' })).toHaveCount(0);
    await model.press('ArrowDown');
    const nano = choices.getByRole('button', { name: /^Nemotron 3 Nano/ });
    await expect(nano).toBeFocused();
    await expect(choices.getByRole('button', { name: /^Nemotron 3 Ultra/ })).toBeDisabled();
    await expect(choices.getByRole('button', { name: /^Nemotron 3 Super/ })).toBeDisabled();
    await expect(choices.getByRole('button', { name: /^Nemotron 3.5 Lightning/ })).toBeDisabled();
    await page.keyboard.press('Home');
    await expect(nano).toBeFocused();
    await page.keyboard.press('End');
    await expect(nano).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(panel).toBeVisible();
    await expect(model).toBeFocused();
    await expect(choices).not.toBeVisible();
    await picker.press('Escape');
    await expect(panel).not.toBeVisible();
    await expect(picker).toBeFocused();
    await settings.click();
    await expect(settingsPanel.getByRole('button', { name: 'Outils indisponibles' })).toBeDisabled();
    for (const name of ['Faible', 'Standard', 'Élevé', 'Maximum']) {
      await expect(settingsPanel.getByRole('button', { name, exact: true })).toBeDisabled();
    }
    await settingsPanel.getByRole('button', { name: '8k', exact: true }).click();
    await expect(settingsPanel.getByRole('button', { name: '8k', exact: true })).toHaveAttribute('aria-pressed', 'true');
    await draft.click();
    await expect(settingsPanel).not.toBeVisible();
    await expect(draft).toHaveValue('Une idée <script>test</script>');
    await expect(page.locator('.user-message')).toHaveCount(0);
    for (const width of [1000, 390, 320]) {
      await page.setViewportSize({ width, height: 900 });
      await picker.click();
      await model.click();
      const bounds = await panel.boundingBox();
      expect(bounds.x).toBeGreaterThanOrEqual(0);
      expect(bounds.x + bounds.width).toBeLessThanOrEqual(width);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await nano.click();
      await expect(picker).toContainText('Nano');
      await picker.press('Escape');
      await settings.click();
      await expect(settingsPanel.getByRole('button', { name: '8k', exact: true })).toHaveAttribute('aria-pressed', 'true');
      await settings.press('Escape');
    }
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await expect(draft).toHaveValue('Une idée <script>test</script>');
    expect(errors).toEqual([]);
  } finally {
    await app?.close();
    const target = resolve(profile);
    if (dirname(target) !== resolve(tmpdir()) || !basename(target).startsWith('kyro-model-picker-')) throw new Error('Unexpected test profile path');
    await rm(target, { recursive: true, force: true });
  }
});
