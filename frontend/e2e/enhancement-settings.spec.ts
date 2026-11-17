import { expect, test } from '@playwright/test';
import { serveProductionApp } from './support/production-app';
import { installUsageDesktop } from './support/usage-desktop';

test('save a Nemotron enhancement default while Whisper stays selected for calls', async ({ page }, testInfo) => {
  await installUsageDesktop(page);
  await page.addInitScript(() => {
    const desktop = (window as any).__TAURI_INTERNALS__;
    const original = desktop.invoke;
    desktop.invoke = async (command: string, args: any) => {
      if (['nemotron_init', 'funasr_init'].includes(command)) return null;
      if (command === 'whisper_get_available_models') return [{ name: 'large-v3', status: 'Available', size_mb: 3000 }];
      if (command === 'nemotron_get_available_models') return localStorage.getItem('testModelMissing') ? [] : [{ name: 'nemotron-q2', display_name: 'Nemotron Q2', status: 'Available', size_mb: 17000, supported: true }];
      if (command === 'funasr_get_available_models') return [];
      return original(command, args);
    };
  });
  await serveProductionApp(page);
  await page.goto('https://meetily.test/settings');
  await page.getByRole('tab', { name: 'Transcription', exact: true }).click();
  const model = page.getByRole('combobox', { name: 'Default enhancement model' });
  await expect(model).toBeEnabled();
  const auto = page.getByRole('switch', { name: 'Auto enhance', exact: true });
  await expect(auto).not.toBeChecked();
  await expect(auto).toBeDisabled();
  await model.click();
  await page.getByRole('option', { name: 'Nemotron: Nemotron Q2' }).click();
  await auto.click();
  await expect(auto).toBeChecked();
  await expect(page.getByRole('combobox').first()).toContainText('Local Whisper');
  await page.reload();
  await page.getByRole('tab', { name: 'Transcription', exact: true }).click();
  await expect(model).toContainText('Nemotron: Nemotron Q2');
  await expect(auto).toBeChecked();
  await auto.scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath('enhancement-settings.png') });
  // A model removed or downloaded while Settings stays open appears on the next open.
  await page.evaluate(() => localStorage.setItem('testModelMissing', 'true'));
  await model.click();
  await expect(page.getByRole('option', { name: 'nemotron-q2 (unavailable)' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(auto).toBeDisabled();
  await page.evaluate(() => localStorage.removeItem('testModelMissing'));
  await model.click();
  await page.getByRole('option', { name: 'Nemotron: Nemotron Q2' }).click();
  await expect(auto).toBeEnabled();
  const calls = await page.evaluate(() => JSON.parse(localStorage.getItem('meetily-usage-fixture')!).calls);
  expect(calls.filter((call: any) => call.command === 'api_save_transcript_config')).toEqual([]);
});

for (const outcome of ['complete', 'error', 'navigate', 'reload']) {
  test(`auto summary waits for enhancement ${outcome}`, async ({ page }) => {
    await installUsageDesktop(page);
    await page.addInitScript(({ outcome }) => {
      localStorage.setItem('isAutoSummary', 'true');
      localStorage.setItem('enhancementPreferences', JSON.stringify({ autoEnhance: true, provider: 'nemotron', model: 'nemotron-q2' }));
      const desktop = (window as any).__TAURI_INTERNALS__;
      const original = desktop.invoke;
      let complete = sessionStorage.getItem('testEnhancementComplete') === 'true';
      let started = sessionStorage.getItem('testEnhancementStarted') === 'true';
      let running = sessionStorage.getItem('testEnhancementRunning') === 'true';
      let summaryInput: unknown = null;
      let refined = false;
      const enhanced = 'The enhanced transcript contains the final decision.';
      Object.assign(window, { __enhancementTest: {
        started: () => started,
        summaryInput: () => summaryInput,
        refined: () => refined,
        async finish() {
          complete = outcome !== 'error';
          running = false;
          sessionStorage.setItem('testEnhancementComplete', String(complete));
          sessionStorage.setItem('testEnhancementRunning', 'false');
          await original('plugin:event|emit', { event: `retranscription-${outcome === 'error' ? 'error' : 'complete'}`, payload: { meeting_id: 'walkthrough-meeting', error: 'Test failure' } });
        },
      } });
      desktop.invoke = async (command: string, args: any = {}) => {
        if (command === 'stop_recording') {
          const result = await original(command, args);
          await original('plugin:event|emit', { event: 'recording-stopped', payload: { folder_path: '/recordings/call', meeting_name: 'Call' } });
          return result;
        }
        if (outcome === 'error' && command === 'get_diarization_settings') return { enabled: true, postCallRefinementEnabled: true, mode: 'post_call_only', showProvisionalLabels: false };
        if (outcome === 'error' && command === 'get_diarization_model_status') return { installed: true, refinementSupported: true };
        if (command === 'start_final_diarization') { refined = true; return {}; }
        if (command === 'nemotron_init') return null;
        if (command === 'nemotron_get_available_models') return [{ name: 'nemotron-q2', status: 'Available' }];
        if (command === 'is_retranscription_in_progress_command') return running && args.meetingId === 'walkthrough-meeting';
        if (command === 'start_retranscription_command') {
          started = true; running = true;
          sessionStorage.setItem('testEnhancementStarted', 'true');
          sessionStorage.setItem('testEnhancementRunning', 'true');
          return { meeting_id: args.meetingId };
        }
        if (command === 'api_get_meeting_metadata' && args.meetingId === 'other-meeting') return {
          id: 'other-meeting', title: 'Other meeting', created_at: '2026-10-28T10:00:00Z', updated_at: '2026-10-28T10:00:00Z', folder_path: null,
        };
        if (command === 'api_process_transcript') summaryInput = args;
        const result = await original(command, args);
        if (outcome === 'navigate' && command === 'api_get_meetings') return [...result, { id: 'other-meeting', title: 'Other meeting' }];
        if (complete && command === 'api_get_meeting_transcripts') return { ...result, transcripts: result.transcripts.map((row: any) => ({ ...row, text: enhanced })) };
        if (complete && command === 'api_get_meeting') return { ...result, transcripts: result.transcripts.map((row: any) => ({ ...row, text: enhanced })) };
        return result;
      };
    }, { outcome });
    await serveProductionApp(page);
    await page.goto('https://meetily.test/');
    await page.getByRole('button', { name: 'Expand sidebar' }).click();
    await page.getByRole('button', { name: 'Start Recording', exact: true }).click();
    await page.getByRole('button', { name: "I've Notified Participants" }).click();
    await page.evaluate(() => (window as any).__meetilyUsageFixture.emitTranscript());
    await page.getByRole('button', { name: 'Stop recording', exact: true }).click();
    await expect(page).toHaveURL(/meeting-details\?id=walkthrough-meeting/, { timeout: 20_000 });
    expect(await page.evaluate(() => (window as any).__enhancementTest.started())).toBe(true);
    expect(await page.evaluate(() => (window as any).__enhancementTest.summaryInput())).toBeNull();
    if (outcome === 'reload') {
      await page.reload();
      await expect(page.getByRole('region', { name: 'Transcript', exact: true })).toBeVisible();
      expect(await page.evaluate(() => (window as any).__enhancementTest.summaryInput())).toBeNull();
    }
    if (outcome === 'navigate') {
      await page.getByText('Other meeting', { exact: true }).first().click();
      await expect(page).toHaveURL(/meeting-details\?id=other-meeting/);
      await expect(page.getByRole('button', { name: 'Edit meeting title' }).first()).toBeVisible();
      await expect(page.getByText('Other meeting', { exact: true }).first()).toBeVisible();
      await page.evaluate(() => (window as any).__enhancementTest.finish());
      // Give the old asynchronous setup time to resume after the event.
      await page.waitForTimeout(300);
      expect(await page.evaluate(() => (window as any).__enhancementTest.summaryInput())).toBeNull();
      return;
    }
    await page.evaluate(() => (window as any).__enhancementTest.finish());
    await expect.poll(() => page.evaluate(() => (window as any).__enhancementTest.summaryInput())).not.toBeNull();
    const input = JSON.stringify(await page.evaluate(() => (window as any).__enhancementTest.summaryInput()));
    if (outcome === 'error') await expect.poll(() => page.evaluate(() => (window as any).__enhancementTest.refined())).toBe(true);
    expect(input).toContain(outcome === 'error' ? 'agreed to review the meeting notes' : 'enhanced transcript contains the final decision');
  });
}
