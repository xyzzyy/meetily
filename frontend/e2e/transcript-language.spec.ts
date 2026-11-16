import { expect, test } from '@playwright/test';
import { serveProductionApp } from './support/production-app';
import { installUsageDesktop } from './support/usage-desktop';

for (const provider of ['localWhisper', 'funasr', 'nemotron', 'parakeet']) {
  test(`transcript language button: ${provider}`, async ({ page }, testInfo) => {
    await installUsageDesktop(page, provider);
    await serveProductionApp(page);
    await page.goto('https://meetily.test/');
    await page.getByRole('button', { name: 'Expand sidebar' }).click();
    await expect(page.getByRole('button', { name: 'Start Recording', exact: true })).toBeVisible();
    const button = page.getByRole('button', { name: 'Language', exact: true });
    if (provider === 'parakeet') {
      await page.getByRole('button', { name: 'Start Recording', exact: true }).click();
      await expect(page.getByRole('combobox')).toContainText('Parakeet');
      await expect(button).toHaveCount(0);
      return;
    }
    await button.click();
    await expect(page.getByRole('combobox')).toContainText('English');
    if (provider === 'nemotron') {
      await expect(page.getByRole('combobox')).toContainText('Auto (No language hint)');
      await page.screenshot({ path: testInfo.outputPath('nemotron-language.png') });
    }
  });
}
