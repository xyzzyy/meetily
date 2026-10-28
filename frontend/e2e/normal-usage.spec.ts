import { expect, test } from '@playwright/test';
import { collectAppErrors, serveProductionApp } from './support/production-app';
import { installUsageDesktop, meetingId, readDesktopState, summaryText, transcriptText } from './support/usage-desktop';

test('normal session: record, pause, save, summarize, edit, reopen, search, and delete', async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(10_000);
  const errors = collectAppErrors(page);
  await installUsageDesktop(page);
  await serveProductionApp(page);
  await page.goto('https://meetily.test/');
  await page.getByRole('button', { name: 'Expand sidebar' }).click();

  await test.step('start a meeting and receive its live transcript', async () => {
    await page.getByRole('button', { name: 'Start Recording', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Pause recording', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: "I've Notified Participants" }).click();
    // Supply an external transcript through the IPC boundary, as the native
    // engine would. All recording state, buffering, and IndexedDB code is real.
    await page.evaluate(() => (window as any).__meetilyUsageFixture.emitTranscript());
    await expect(page.getByText(transcriptText, { exact: true })).toBeVisible();
  });

  await test.step('pause, resume, and stop the recording', async () => {
    await page.getByRole('button', { name: 'Pause recording', exact: true }).click();
    await page.getByRole('button', { name: 'Resume recording', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Pause recording', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Stop recording', exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`/meeting-details\\?id=${meetingId}`), { timeout: 20_000 });
    await expect(page.getByRole('region', { name: 'Transcript', exact: true })).toContainText(transcriptText);
    const state = await readDesktopState(page);
    expect(state.meeting.transcripts).toHaveLength(1);
    expect(state.meeting.transcripts[0].text).toBe(transcriptText);
  });

  await test.step('generate, edit, and save meeting notes', async () => {
    await page.getByTitle('Generate AI Summary', { exact: true }).click();
    const editor = page.locator('.bn-editor');
    await expect(editor).toContainText(summaryText, { timeout: 15_000 });
    await editor.locator('.transcript-ref').click();
    await expect(page.getByRole('region', { name: 'Transcript', exact: true })).toContainText(transcriptText);
    await editor.click();
    await page.keyboard.press('End');
    await page.keyboard.type(' Bring the agenda.');
    await page.getByTitle('Save Changes', { exact: true }).click();
    await expect.poll(async () => JSON.stringify((await readDesktopState(page)).summary)).toContain('Bring the agenda.');
  });

  await test.step('rename and reopen the saved meeting', async () => {
    await page.getByRole('button', { name: 'Edit meeting title' }).click();
    await page.getByLabel('Meeting Title', { exact: true }).fill('Friday planning');
    await page.getByRole('dialog').getByRole('button', { name: 'Save', exact: true }).click();
    await page.reload();
    await page.getByRole('button', { name: 'Expand sidebar' }).click();
    await expect(page.getByRole('region', { name: 'Transcript', exact: true })).toContainText(transcriptText);
    await expect(page.locator('.bn-editor')).toContainText('Bring the agenda.');
    await expect(page.getByText('Friday planning', { exact: true }).first()).toBeVisible();
  });

  await test.step('find the meeting by its transcript and delete it', async () => {
    await page.getByPlaceholder('Search meeting content...').fill('Friday');
    await expect.poll(async () => (await readDesktopState(page)).calls
      .some((call: any) => call.command === 'api_search_transcripts' && call.args.query === 'Friday')).toBe(true);
    await page.getByText('Friday planning', { exact: true }).first().click();
    await page.getByRole('button', { name: 'Delete meeting', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Confirm Delete' })).toBeVisible();
    await page.getByRole('button', { name: 'Delete', exact: true }).click();
    await expect(page.getByText('Friday planning', { exact: true })).toHaveCount(0);
    expect((await readDesktopState(page)).meeting).toBeNull();
    await page.reload();
    await page.getByRole('button', { name: 'Expand sidebar' }).click();
    await expect(page.getByRole('button', { name: 'Start Recording', exact: true })).toBeVisible();
    expect((await readDesktopState(page)).meeting).toBeNull();
  });

  expect((await readDesktopState(page)).unexpected).toEqual([]);
  expect(errors).toEqual([]);
});

test.afterEach(async ({ page }, testInfo) => {
  if (testInfo.status !== testInfo.expectedStatus) {
    await testInfo.attach('desktop-state', {
      body: JSON.stringify(await readDesktopState(page), null, 2), contentType: 'application/json',
    });
  }
});
