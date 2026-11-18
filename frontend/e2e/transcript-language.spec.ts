import { expect, test } from '@playwright/test';
import { serveProductionApp } from './support/production-app';
import { installUsageDesktop, readDesktopState } from './support/usage-desktop';

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
      await expect(page.getByRole('combobox', { name: 'Transcript model', exact: true })).toContainText('Parakeet');
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

test('Nemotron settings offer Q2_0_M and save it after download', async ({ page }, testInfo) => {
  await installUsageDesktop(page, 'nemotron');
  await serveProductionApp(page);
  await page.goto('https://meetily.test/settings');
  await page.getByRole('tab', { name: 'Transcription', exact: true }).click();
  const model = page.getByRole('button', { name: /^Nemotron 3\.5 Transcribe · Q2_0_M/ });
  const q4 = page.getByRole('button', { name: /^Nemotron 3\.5 Transcribe · IQ4_NL/ });
  await expect(q4).toBeVisible();
  await expect(q4).toBeEnabled();
  await expect(model).toContainText('15.2 GB download');
  await expect(model).toBeDisabled();
  await expect(page.getByRole('button', { name: /Nemotron.*Q2_0_L/ })).toHaveCount(0);
  await expect(page.getByText(/Q2_0_M needs about 15\.2 GB/)).toBeVisible();
  await page.getByRole('button', { name: 'Download', exact: true }).click();
  await expect(model).toBeEnabled();
  await model.click();
  await expect.poll(async () => (await readDesktopState(page)).calls.some((call: any) =>
    call.command === 'api_save_transcript_config' &&
    call.args.provider === 'nemotron' && call.args.model === 'nemotron-3.5-transcribe-q2_0_m'
  )).toBe(true);
  await expect(model).toHaveAttribute('aria-pressed', 'true');
  await page.screenshot({ path: testInfo.outputPath('nemotron-q2-m-settings.png'), fullPage: true });
});
