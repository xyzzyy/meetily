import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import path from 'node:path';

const outputDirectory = path.resolve(__dirname, '../out');
const transcriptText = 'We agreed to review the meeting notes on Friday.';
const summaryText = 'Review the meeting notes. [00:00-00:04]';
const contentTypes: Record<string, string> = {
  '.html': 'text/html', '.txt': 'text/plain', '.js': 'application/javascript',
  '.css': 'text/css', '.woff2': 'font/woff2', '.json': 'application/json',
};

async function openMeeting(page: Page, {
  format = 'markdown', segmentCount = 1, summary = summaryText, longRowIndex, ollamaModels = [],
}: {
  format?: 'markdown' | 'blocknote' | 'empty';
  segmentCount?: number;
  summary?: string;
  longRowIndex?: number;
  ollamaModels?: { id: string; name: string; size: string; modified: string }[];
} = {}) {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    // React reports caught render failures to console.error instead of pageerror.
    page.on('console', message => {
      if (message.type() === 'error') errors.push(message.text());
    });

    // Exercise the real production chunks. Only the desktop IPC boundary is
    // replaced; no React components, editors, or extensions are mocked.
    await page.addInitScript(({ format, transcriptText, summaryText, segmentCount, longRowIndex, ollamaModels }) => {
      const meetingTitles = new Map([['saved-meeting', 'Saved planning meeting'], ['other-meeting', 'Other planning meeting']]);
      let callbackId = 0;
      const callbacks = new Map<number, unknown>();
      const store = new Map<string, unknown>([
        ['analyticsOptedIn', false],
        ['analyticsDefaultOffMigrationV1', true],
      ]);
      Object.assign(window, {
        __TAURI_OS_PLUGIN_INTERNALS__: {
          platform: 'macos', version: '15.0', family: 'unix', os_type: 'macos',
          arch: 'aarch64', eol: '\n', exe_extension: '',
        },
        __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener() {} },
        __TAURI_INTERNALS__: {
          metadata: {
            currentWindow: { label: 'main' },
            currentWebview: { label: 'main', windowLabel: 'main' },
          },
          transformCallback(callback: unknown) {
            callbacks.set(++callbackId, callback);
            return callbackId;
          },
          unregisterCallback(id: number) { callbacks.delete(id); },
          convertFileSrc(file: string) { return file; },
          async invoke(command: string, args: Record<string, any> = {}) {
            switch (command) {
              case 'plugin:event|listen': return ++callbackId;
              case 'plugin:event|unlisten':
              case 'plugin:window|set_theme':
              case 'set_language_preference':
              case 'plugin:store|save':
              case 'plugin:resources|close': return null;
              case 'plugin:store|load': return 1;
              case 'plugin:store|has': return store.has(args.key);
              case 'plugin:store|get': return [store.get(args.key), store.has(args.key)];
              case 'plugin:store|set': store.set(args.key, args.value); return null;
              case 'get_onboarding_status': return {
                completed: true, current_step: 1,
                model_status: { parakeet: 'downloaded', summary: 'downloaded' },
              };
              case 'check_first_launch':
              case 'is_recording': return false;
              case 'plugin:app|version': return '0.4.1';
              case 'api_get_meetings': return [...meetingTitles].map(([id, title]) => ({ id, title }));
              case 'api_delete_meeting': meetingTitles.delete(args.meetingId); return null;
              case 'api_save_meeting_title': meetingTitles.set(args.meetingId, args.title); return null;
              case 'api_get_auto_generate_setting': return false;
              case 'api_get_meeting_metadata': return {
                id: args.meetingId, title: meetingTitles.get(args.meetingId) || 'Saved planning meeting',
                created_at: '2026-10-27T10:00:00Z', updated_at: '2026-10-27T10:00:00Z',
                folder_path: null,
              };
              case 'api_get_meeting_transcripts': {
                const offset = args.offset ?? 0;
                const count = Math.min(args.limit ?? 100, segmentCount - offset);
                return {
                  transcripts: Array.from({ length: count }, (_, index) => {
                    const number = offset + index;
                    return {
                      id: `transcript-${number + 1}`,
                      text: number === longRowIndex ? 'A long supporting transcript passage. '.repeat(180)
                        : number === 0 ? transcriptText : `Transcript row ${number + 1}. ${'More meeting detail. '.repeat(number % 3)}`,
                      timestamp: '10:00:00', audio_start_time: number * 10,
                      audio_end_time: number * 10 + 8, confidence: 0.98,
                      speaker_label: 'Speaker 1',
                    };
                  }),
                  has_more: offset + count < segmentCount, total_count: segmentCount,
                };
              }
              case 'api_get_summary': return {
                meeting_id: args.meetingId, meetingName: 'Saved planning meeting',
                status: format === 'empty' ? 'idle' : 'completed',
                start: null, end: null, error: null,
                data: format === 'empty' ? null : format === 'markdown'
                  ? { markdown: summaryText }
                  : { summary_json: [{
                    id: 'summary-1', type: 'paragraph',
                    content: [{ type: 'text', text: summaryText, styles: {} }],
                  }] },
              };
              case 'api_get_meeting_summary_language': return { language: null, storage: 'metadata' };
              case 'api_get_transcript_config': return { provider: 'localWhisper', model: 'large-v3', apiKey: null };
              case 'api_get_model_config': return { provider: 'ollama', model: 'test-model', whisperModel: 'large-v3' };
              case 'api_get_api_key':
              case 'get_meeting_diarization_status': return null;
              case 'get_recording_preferences': return { preferred_mic_device: null, preferred_system_device: null };
              case 'get_diarization_settings': return { enabled: false, mode: 'off', showProvisionalLabels: false };
              case 'get_diarization_model_status': return { installed: false, refinementSupported: false };
              case 'get_recording_state': return { is_recording: false, is_paused: false };
              case 'get_audio_devices': return [{ name: 'Test microphone', device_type: 'Input' }];
              case 'get_ollama_models': return ollamaModels;
              case 'get_meeting_speakers':
              case 'list_speaker_profiles':
              case 'api_list_templates': return [];
              default: throw new Error(`Unexpected desktop command: ${command}`);
            }
          },
        },
      });
    }, { format, transcriptText, summaryText: summary, segmentCount, longRowIndex, ollamaModels });

    // Serve the static export through request interception, with no dev server.
    await page.route('https://meetily.test/**', async route => {
      const url = new URL(route.request().url());
      let filename = path.join(outputDirectory, decodeURIComponent(url.pathname));
      if (!path.extname(filename)) filename += url.pathname.endsWith('/') ? 'index.html' : '.html';
      const contentType = contentTypes[path.extname(filename)] ?? 'application/octet-stream';
      await route.fulfill({ body: await readFile(filename), contentType });
    });

    await page.goto('https://meetily.test/meeting-details?id=saved-meeting');
    await expect(page.getByRole('region', { name: 'Transcript', exact: true }).getByText(transcriptText, { exact: true })).toBeVisible();
    await expect(page.locator('main .h-screen').last()).toHaveCSS('transform', 'none');
    return errors;
}

for (const format of ['markdown', 'blocknote', 'empty'] as const) {
  test(`opens a saved meeting with ${format} summary content`, async ({ page }) => {
    const errors = await openMeeting(page, { format });
    await expect(page.getByText(transcriptText, { exact: true })).toBeVisible();
    if (format === 'empty') {
      await expect(page.getByText('No Summary Generated Yet', { exact: true })).toBeVisible();
    } else {
      const editor = page.locator('.bn-editor');
      await expect(editor).toBeVisible();
      await expect(editor).toContainText(summaryText);
      await expect(editor).toHaveAttribute('contenteditable', 'true');
      await editor.locator('.transcript-ref').click();
      await expect(page.locator('#segment-transcript-1')).toHaveAttribute('data-highlighted', 'true');
      await editor.click();
      await page.keyboard.press('End');
      await page.keyboard.type(' Updated');
      await expect(editor).toContainText('Updated');
    }
    await expect(page.getByText('Application error:', { exact: false })).toHaveCount(0);
    expect(errors).toEqual([]);
  });
}

async function transcriptPosition(page: Page, targetId: string) {
  return page.locator(`#${targetId}`).evaluate(row => {
    const container = row.closest('[data-transcript-scroll-container]') as HTMLElement;
    const rect = row.getBoundingClientRect();
    const viewport = container.getBoundingClientRect();
    const text = row.querySelector('p')!.getBoundingClientRect();
    return {
      scrollTop: container.scrollTop,
      scrollHeight: container.scrollHeight,
      left: rect.left - viewport.left,
      top: rect.top - viewport.top,
      width: rect.width,
      height: rect.height,
      textLeft: text.left - viewport.left,
      textTop: text.top - viewport.top,
    };
  });
}

function expectSamePosition(actual: Record<string, number>, expected: Record<string, number>) {
  for (const key of Object.keys(expected)) expect(actual[key], key).toBeCloseTo(expected[key], 0);
}

for (const segmentCount of [4, 130]) {
  test(`highlight keeps visible transcript rows in place (${segmentCount} rows)`, async ({ page }, testInfo) => {
    await page.emulateMedia({ colorScheme: segmentCount === 130 ? 'dark' : 'light' });
    const errors = await openMeeting(page, { segmentCount, summary: 'Follow-up [00:10-00:18]' });
    const target = page.locator('#segment-transcript-2');
    await expect(target).toBeVisible();
    const before = await transcriptPosition(page, 'segment-transcript-2');
    await page.locator('.transcript-ref').click();
    await expect(target).toHaveAttribute('data-highlighted', 'true');
    await expect(target).toHaveCSS('box-shadow', 'none');
    expectSamePosition(await transcriptPosition(page, 'segment-transcript-2'), before);
    await page.screenshot({ path: testInfo.outputPath('reference-highlight.png') });
    await expect(target).not.toHaveAttribute('data-highlighted', 'true', { timeout: 1500 });
    expectSamePosition(await transcriptPosition(page, 'segment-transcript-2'), before);
    expect(errors).toEqual([]);
  });
}

test('a reference on a later page scrolls only the transcript and stays put on a second click', async ({ page }) => {
  const errors = await openMeeting(page, { segmentCount: 130, summary: 'Later discussion [20:00-20:08]' });
  const summary = page.getByRole('region', { name: 'Summary', exact: true });
  const before = await summary.boundingBox();
  await page.locator('.transcript-ref').click();
  const target = page.locator('#segment-transcript-121');
  await expect(target).toHaveAttribute('data-highlighted', 'true');
  await expect(target).toBeInViewport({ ratio: 1 });
  expect(await summary.boundingBox()).toEqual(before);
  const position = await transcriptPosition(page, 'segment-transcript-121');
  expect(position.scrollTop).toBeGreaterThan(0);
  await page.locator('.transcript-ref').click();
  expectSamePosition(await transcriptPosition(page, 'segment-transcript-121'), position);
  expect(errors).toEqual([]);
});

test('action item tables keep supporting text separate from the timestamp', async ({ page }) => {
  const errors = await openMeeting(page, { summary: [
    '| Owner | Task | Due | Reference Transcript Segment | Segment Time stamp |',
    '| --- | --- | --- | --- | --- |',
    '| Andrew | Review notes | Friday | We agreed to review the meeting notes on Friday. | [00:00-00:04] |',
  ].join('\n') });
  const table = page.locator('.bn-editor table');
  const cells = table.locator('tr').nth(1).locator('td');
  await expect(cells).toHaveCount(5);
  await expect(cells.nth(3)).toContainText(transcriptText);
  await expect(cells.nth(3).locator('.transcript-ref')).toHaveCount(0);
  await expect(cells.nth(4).locator('.transcript-ref')).toHaveCount(1);
  await cells.nth(4).locator('.transcript-ref').click();
  await expect(page.locator('#segment-transcript-1')).toHaveAttribute('data-highlighted', 'true');
  expect(errors).toEqual([]);
});

for (const { segmentCount, longRowIndex, timestamp } of [
  { segmentCount: 4, longRowIndex: 3, timestamp: '00:30' },
  { segmentCount: 130, longRowIndex: 120, timestamp: '20:00' },
]) {
  test(`long reference rows reveal their timestamp (${segmentCount} rows)`, async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 400 });
    const errors = await openMeeting(page, {
      segmentCount, longRowIndex, summary: `Long discussion [${timestamp}]`,
    });
    const target = page.locator(`#segment-transcript-${longRowIndex + 1}`);
    const label = target.getByText(`[${timestamp}]`, { exact: true });
    await expect(label).not.toBeInViewport();
    await page.locator('.transcript-ref').click();
    await expect(label).toBeInViewport({ ratio: 1 });
    const container = page.locator('[data-transcript-scroll-container]');
    await container.evaluate(element => { element.scrollTop += 40; });
    const position = await transcriptPosition(page, `segment-transcript-${longRowIndex + 1}`);
    await page.locator('.transcript-ref').click();
    expectSamePosition(await transcriptPosition(page, `segment-transcript-${longRowIndex + 1}`), position);
    expect(errors).toEqual([]);
  });
}


test('keyboard navigation, title editing, and delete confirmation preserve focus', async ({ page, browserName }) => {
  // WebKit follows macOS policy: Option+Tab includes native buttons.
  const tab = (reverse = false) => page.keyboard.press(`${reverse ? 'Shift+' : ''}${browserName === 'webkit' ? 'Alt+' : ''}Tab`);
  const errors = await openMeeting(page);
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).focus();
  await page.keyboard.press('Enter');
  const other = page.getByRole('button', { name: 'Other planning meeting', exact: true });
  await other.focus(); await page.keyboard.press('Space');
  await expect(page).toHaveURL(/id=other-meeting/);
  await expect(other).toHaveAttribute('aria-current', 'page');
  await other.focus(); await tab();
  const edit = page.getByRole('button', { name: 'Edit meeting title', exact: true }).last();
  await expect(edit).toBeFocused(); await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog').getByLabel('Meeting Title')).toBeFocused();
  await page.keyboard.press('Escape'); await expect(edit).toBeFocused();
  await tab();
  const remove = page.getByRole('button', { name: 'Delete meeting', exact: true }).last();
  await expect(remove).toBeFocused(); await page.keyboard.press('Enter');
  const confirmation = page.getByRole('dialog', { name: 'Confirm Delete' });
  await expect(confirmation.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
  await tab(true);
  expect(await confirmation.evaluate(dialog => dialog.contains(document.activeElement))).toBe(true);
  await page.keyboard.press('Escape'); await expect(remove).toBeFocused();
  await page.keyboard.press('Enter'); await tab();
  await expect(confirmation.getByRole('button', { name: 'Delete', exact: true })).toBeFocused();
  await page.keyboard.press('Enter'); await expect(confirmation).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Collapse sidebar', exact: true })).toBeFocused();
  await expect(other).toHaveCount(0);
  expect(errors).toEqual([]);
});

for (const format of ['markdown', 'blocknote'] as const) {
  test(`Alt+Enter follows the reference under the caret without changing ${format} text`, async ({ page }) => {
    const errors = await openMeeting(page, { format });
    const editor = page.locator('.bn-editor');
    const before = await editor.innerText();
    await editor.locator('.transcript-ref').first().evaluate(reference => {
      (reference.closest('.bn-editor') as HTMLElement).focus();
      const range = document.createRange(); range.setStart(reference.firstChild!, 3); range.collapse(true);
      const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
    });
    await page.keyboard.press('Alt+Enter');
    await expect(page.locator('#segment-transcript-1')).toHaveAttribute('data-highlighted', 'true');
    await expect(page.getByRole('region', { name: 'Transcript text', exact: true })).toBeFocused();
    expect(await editor.innerText()).toBe(before);
    await editor.focus(); await page.keyboard.press('ControlOrMeta+End'); await page.keyboard.press('Space');
    expect(await editor.evaluate(element => element.contains(document.activeElement))).toBe(true);
    await page.keyboard.press('Enter'); expect(await editor.innerText()).not.toBe(before);
    expect(errors).toEqual([]);
  });
}

test('summary settings sections and password visibility work with the keyboard', async ({ page }) => {
  const errors = await openMeeting(page);
  await page.getByTitle('Summary Settings').focus(); await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog');
  const endpoint = dialog.getByRole('button', { name: 'Custom Endpoint (optional)', exact: true });
  await endpoint.focus(); await page.keyboard.press('Space');
  await expect(endpoint).toHaveAttribute('aria-expanded', 'true');
  await dialog.getByRole('combobox').first().focus(); await page.keyboard.press('Enter');
  await page.getByRole('option', { name: 'OpenAI', exact: true }).press('Enter');
  await expect(page.getByRole('listbox')).toHaveCount(0);
  await expect(dialog.getByRole('combobox').first()).toBeFocused();
  const visibility = dialog.getByRole('button', { name: 'Show API key', exact: true });
  await visibility.focus(); await page.keyboard.press('Space');
  await expect(dialog.getByPlaceholder('Enter your API key')).toHaveAttribute('type', 'text');
  await page.keyboard.press('Enter');
  await expect(dialog.getByPlaceholder('Enter your API key')).toHaveAttribute('type', 'password');
  expect(errors).toEqual([]);
});


test('language picker focuses search and returns focus on Escape', async ({ page }) => {
  const errors = await openMeeting(page);
  const trigger = page.getByRole('button', { name: 'Set summary language' });
  await trigger.focus(); await page.keyboard.press('Enter');
  const picker = page.getByRole('dialog', { name: 'Pick summary language' });
  await expect(picker.getByRole('textbox', { name: 'Search summary languages' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(picker).toHaveCount(0); await expect(trigger).toBeFocused();
  expect(errors).toEqual([]);
});

test('Ollama model selection works with Enter and Space', async ({ page }) => {
  const errors = await openMeeting(page, { ollamaModels: [
    { id: 'one', name: 'test-model', size: '1 GB', modified: 'today' },
    { id: 'two', name: 'other-model', size: '2 GB', modified: 'today' },
  ] });
  await page.getByTitle('Summary Settings').focus(); await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog');
  const first = dialog.getByRole('button', { name: /test-model with a size of/ });
  const second = dialog.getByRole('button', { name: /other-model with a size of/ });
  await expect(first).toHaveAttribute('aria-pressed', 'true');
  await second.focus(); await page.keyboard.press('Space');
  await expect(second).toHaveAttribute('aria-pressed', 'true');
  await expect(first).toHaveAttribute('aria-pressed', 'false');
  await first.focus(); await page.keyboard.press('Enter');
  await expect(first).toHaveAttribute('aria-pressed', 'true');
  await expect(second).toHaveAttribute('aria-pressed', 'false');
  expect(errors).toEqual([]);
});
