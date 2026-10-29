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
  format = 'markdown', segmentCount = 1, summary = summaryText, longRowIndex, enhancement = false, recording = false,
}: {
  format?: 'markdown' | 'blocknote' | 'empty';
  segmentCount?: number;
  summary?: string;
  longRowIndex?: number;
  enhancement?: boolean;
  recording?: boolean;
} = {}) {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    // React reports caught render failures to console.error instead of pageerror.
    page.on('console', message => {
      if (message.type() === 'error') errors.push(message.text());
    });

    // Exercise the real production chunks. Only the desktop IPC boundary is
    // replaced; no React components, editors, or extensions are mocked.
    await page.addInitScript(({ format, transcriptText, summaryText, segmentCount, longRowIndex, enhancement, recording }) => {
      let enhanced = false;
      const controls = { finish: () => {} };
      Object.assign(window, { __ENHANCEMENT_TEST__: controls });
      let callbackId = 0;
      const callbacks = new Map<number, unknown>();
      const listeners = new Map<number, { event: string; handler: number }>();
      const emit = (event: string, payload: unknown) => {
        for (const [id, listener] of listeners) {
          if (listener.event === event) (callbacks.get(listener.handler) as Function)?.({ event, id, payload });
        }
      };
      let trimmed = false;
      let transcribed = false;
      let trimCancelled = false;
      let finishTrim: ((failure: boolean) => void) | null = null;
      const trimCalls: unknown[] = [];
      (window as any).__trimTest = { calls: trimCalls, finish: (failure = false) => finishTrim?.(failure) };
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
              case 'plugin:event|listen': {
                const id = ++callbackId;
                listeners.set(id, { event: args.event, handler: args.handler });
                return id;
              }
              case 'plugin:event|unlisten': listeners.delete(args.eventId); return null;
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
              case 'check_first_launch': return false;
              case 'plugin:app|version': return '0.4.1';
              case 'api_get_meetings': return [{ id: 'saved-meeting', title: 'Saved planning meeting' }, ...(enhancement ? [{ id: 'other-meeting', title: 'Other planning meeting' }] : [])];
              case 'api_get_meeting_metadata': return {
                id: args.meetingId, title: 'Saved planning meeting',
                created_at: '2026-10-27T10:00:00Z', updated_at: '2026-10-27T10:00:00Z',
                folder_path: recording ? '/recordings/saved-meeting' : enhancement ? '/recording' : null,
              };
              case 'api_get_meeting_transcripts': {
                if (trimmed) return { transcripts: transcribed ? [{ id: 'new-transcript', text: 'Only the selected meeting audio.', timestamp: '00:00', audio_start_time: 0, audio_end_time: 40 }] : [], has_more: false, total_count: transcribed ? 1 : 0 };
                const offset = args.offset ?? 0;
                const count = Math.min(args.limit ?? 100, segmentCount - offset);
                return {
                  transcripts: Array.from({ length: count }, (_, index) => {
                    const number = offset + index;
                    return {
                      id: `transcript-${number + 1}`,
                      text: number === longRowIndex ? 'A long supporting transcript passage. '.repeat(180)
                        : number === 0 ? (enhanced && args.meetingId === 'saved-meeting' ? 'Enhanced transcript after background work.' : transcriptText) : `Transcript row ${number + 1}. ${'More meeting detail. '.repeat(number % 3)}`,
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
                status: format === 'empty' || trimmed ? 'idle' : 'completed',
                start: null, end: null, error: null,
                data: format === 'empty' || trimmed ? null : format === 'markdown'
                  ? { markdown: summaryText }
                  : { summary_json: [{
                    id: 'summary-1', type: 'paragraph',
                    content: [{ type: 'text', text: summaryText, styles: {} }],
                  }] },
              };
              case 'api_get_meeting_summary_language': return { language: null, storage: 'metadata' };
              case 'whisper_init':
              case 'parakeet_init':
              case 'funasr_init': return null;
              case 'whisper_get_available_models': return [{ name: 'large-v3', size_mb: 100, status: 'Available' }];
              case 'parakeet_get_available_models':
              case 'funasr_get_available_models': return [];
              case 'start_retranscription_command': return new Promise(resolve => {
                controls.finish = () => { enhanced = true; resolve({ segments_count: 1, duration_seconds: 8 }); };
              });
              case 'api_get_transcript_config': return { provider: 'localWhisper', model: 'large-v3', apiKey: null };
              case 'api_get_model_config': return { provider: 'ollama', model: 'test-model', whisperModel: 'large-v3' };
              case 'api_get_api_key':
              case 'get_meeting_diarization_status': return null;
              case 'get_recording_preferences': return { preferred_mic_device: null, preferred_system_device: null };
              case 'get_diarization_settings': return { enabled: false, mode: 'off', showProvisionalLabels: false };
              case 'get_diarization_model_status': return { installed: false, refinementSupported: false };
              case 'get_recording_state': return { is_recording: false, is_paused: false };
              case 'whisper_init':
              case 'parakeet_init':
              case 'funasr_init': return null;
              case 'whisper_get_available_models': return [{ name: 'large-v3', size_mb: 100, status: 'Available' }];
              case 'parakeet_get_available_models':
              case 'funasr_get_available_models': return [];
              case 'get_recording_trim_info': return 120;
              case 'cancel_retranscription_command': trimCancelled = true; return null;
              case 'trim_and_retranscribe_command':
              case 'start_retranscription_command': {
                trimCalls.push({ command, args });
                trimCancelled = false;
                if (command === 'trim_and_retranscribe_command') {
                  trimmed = true;
                  emit('recording-trimmed', { meeting_id: args.meetingId });
                }
                return new Promise((resolve, reject) => {
                  finishTrim = failure => {
                    if (failure || trimCancelled) {
                      const error = `The recording was trimmed. New transcription did not finish: ${trimCancelled ? 'Retranscription cancelled' : 'Test model failure'}. Use Enhance to try transcription again.`;
                      emit('retranscription-error', { meeting_id: args.meetingId, error });
                      reject(error);
                    } else {
                      transcribed = true;
                      const result = { meeting_id: args.meetingId, segments_count: 1, duration_seconds: 40, language: null };
                      emit('retranscription-complete', result);
                      resolve(result);
                    }
                  };
                });
              }
              case 'get_ollama_models':
              case 'get_meeting_speakers':
              case 'list_speaker_profiles':
              case 'api_list_templates': return [];
              default: throw new Error(`Unexpected desktop command: ${command}`);
            }
          },
        },
      });
    }, { format, transcriptText, summaryText: summary, segmentCount, longRowIndex, enhancement, recording });

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


test('enhancement runs while the user closes its dialog and visits another meeting', async ({ page }) => {
  const errors = await openMeeting(page, { enhancement: true });
  const enhance = page.getByTitle('Retranscribe to enhance your recorded audio');
  await enhance.click();
  await page.getByRole('button', { name: 'Start enhancement', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByText('Enhancing meeting', { exact: true })).toBeVisible();
  await enhance.click();
  await expect(page.getByRole('button', { name: 'Run in background' })).toBeVisible();
  await page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  // The sidebar starts collapsed, so expand it before selecting another meeting.
  await page.locator('.fixed.top-0.left-0.h-screen > button').click();
  await page.getByText('Other planning meeting', { exact: true }).click();
  await expect(page).toHaveURL(/id=other-meeting/);
  await page.evaluate(() => (window as any).__ENHANCEMENT_TEST__.finish());
  await expect(page.getByText('Enhancement complete. 1 segments created.', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'View meeting', exact: true }).click();
  await expect(page).toHaveURL(/id=saved-meeting/);
  await expect(page.getByText('Enhanced transcript after background work.', { exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test('an enhancement dialog reopened after navigation is ready for another job when work finishes', async ({ page }) => {
  const errors = await openMeeting(page, { enhancement: true });
  const enhance = page.getByTitle('Retranscribe to enhance your recorded audio');
  await enhance.click();
  await page.getByRole('button', { name: 'Start enhancement', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.locator('.fixed.top-0.left-0.h-screen > button').click();
  await page.getByText('Other planning meeting', { exact: true }).click();
  await expect(page).toHaveURL(/id=other-meeting/);
  await page.locator('.fixed.top-0.left-0.h-screen').getByText('Saved planning meeting', { exact: true }).click();
  await expect(page).toHaveURL(/id=saved-meeting/);
  await enhance.click();
  await expect(page.getByRole('button', { name: 'Run in background' })).toBeVisible();
  await page.evaluate(() => (window as any).__ENHANCEMENT_TEST__.finish());
  await expect(page.getByRole('button', { name: 'Start enhancement', exact: true })).toBeEnabled();
  await expect(page.getByText('The saved model (large-v3) is not available.', { exact: false })).toHaveCount(0);
  const resultNotice = page.getByText('Enhancement complete. 1 segments created.', { exact: true });
  await expect(resultNotice).toBeVisible();
  await page.mouse.move(0, 0);
  await expect(resultNotice).toHaveCount(0, { timeout: 10000 });

for (const failure of [false, true]) {
  test(`trims a saved recording and ${failure ? 'can retry text creation' : 'shows the new transcript'}`, async ({ page }) => {
    const errors = await openMeeting(page, { recording: true });
    await page.getByRole('button', { name: 'Trim saved recording', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog.getByRole('heading', { name: 'Trim recording', exact: true })).toBeVisible();
    await expect(dialog).toContainText('This cannot be undone.');
    const start = dialog.getByRole('spinbutton', { name: 'Start (seconds)', exact: true });
    const end = dialog.getByRole('spinbutton', { name: 'End (seconds)', exact: true });
    const submit = dialog.getByRole('button', { name: 'Trim and transcribe', exact: true });
    await expect(end).toHaveValue('120');
    await expect(submit).toBeDisabled();
    await start.fill('70');
    await end.fill('50');
    await expect(submit).toBeDisabled();
    await start.fill('10');
    await expect(dialog.getByRole('status')).toContainText('Keep 40.000 seconds.');
    await expect(submit).toBeEnabled();
    await submit.click();
    await expect(page.getByText(transcriptText, { exact: true })).toHaveCount(0);
    await expect(page.locator('.bn-editor')).toHaveCount(0);
    const calls = await page.evaluate(() => (window as any).__trimTest.calls);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual({ command: 'trim_and_retranscribe_command', args: {
      meetingId: 'saved-meeting', range: { start_seconds: 10, end_seconds: 50 },
      language: null, model: 'large-v3', provider: 'whisper', assignSpeakers: false,
    } });
    await page.evaluate(fail => (window as any).__trimTest.finish(fail), failure);
    if (failure) {
      await expect(dialog).toContainText('The recording was trimmed. New transcription did not finish');
      await dialog.getByRole('button', { name: 'Try Again', exact: true }).click();
      await expect(dialog.getByRole('spinbutton')).toHaveCount(0);
      await dialog.getByRole('button', { name: 'Transcribe trimmed audio', exact: true }).click();
      const retry = await page.evaluate(() => (window as any).__trimTest.calls[1]);
      expect(retry.command).toBe('start_retranscription_command');
      expect(retry.args.range).toBeUndefined();
      await page.evaluate(() => (window as any).__trimTest.finish());
    }
    await expect(dialog).toHaveCount(0);
    await expect(page.getByText('Only the selected meeting audio.', { exact: true })).toBeVisible();
    await expect(page.getByText(transcriptText, { exact: true })).toHaveCount(0);
    expect(errors).toEqual([]);
  });
}

test('trim cancellation waits for the native job and keeps the saved trim result visible', async ({ page }) => {
  const errors = await openMeeting(page, { recording: true });
  await page.getByRole('button', { name: 'Trim saved recording', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('spinbutton', { name: 'Start (seconds)', exact: true }).fill('10');
  await dialog.getByRole('button', { name: 'Trim and transcribe', exact: true }).click();
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(dialog.getByRole('button', { name: 'Stopping...', exact: true })).toBeDisabled();
  await expect(dialog).toBeVisible();
  await page.evaluate(() => (window as any).__trimTest.finish());
  await expect(dialog).toContainText('The recording was trimmed. New transcription did not finish: Retranscription cancelled');
  await expect(page.getByText(transcriptText, { exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});
