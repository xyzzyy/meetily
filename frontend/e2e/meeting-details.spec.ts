import { expect, test, type Page } from '@playwright/test';
import { collectAppErrors, serveProductionApp } from './support/production-app';

const transcriptText = 'We agreed to review the meeting notes on Friday.';
const summaryText = 'Review the meeting notes. [00:00-00:04]';

async function openMeeting(page: Page, {
  format = 'markdown', segmentCount = 1, summary = summaryText, longRowIndex,
  summaryProvider = 'ollama',
}: {
  format?: 'markdown' | 'blocknote' | 'empty';
  segmentCount?: number;
  summary?: string;
  longRowIndex?: number;
  summaryProvider?: 'ollama' | 'openai' | 'custom-openai';
} = {}) {
    const errors = collectAppErrors(page);

    // Exercise the real production chunks. Only the desktop IPC boundary is
    // replaced; no React components, editors, or extensions are mocked.
    await page.addInitScript(({ format, transcriptText, summaryText, segmentCount, longRowIndex, summaryProvider }) => {
      let callbackId = 0;
      const callbacks = new Map<number, unknown>();
      const testSettings = {
        summaryKey: summaryProvider === 'openai' ? 'saved-test-key' : null as string | null,
        confirmRemoval: false,
        deletedProviders: [] as string[],
        customConfig: { endpoint: 'http://old-server:8000/v1', model: 'old-model', apiKey: null as string | null },
        modelRequests: [] as unknown[],
        modelDiscoveryError: false,
        savedModelConfig: null as unknown,
      };
      const listeners = new Map<number, { event: string; handler: number }>();
      let modelsReady = false;
      let downloadQueries = 0;
      const store = new Map<string, unknown>();
      Object.assign(window, {
        __MEETILY_TEST_SETTINGS__: testSettings,
        __downloadTest: {
          setReady(value: boolean) { modelsReady = value; },
          startMeasurement(available = true, modelName?: string) {
            const timing = { started: performance.now(), completed: null as number | null, removed: null as number | null };
            const observer = new MutationObserver(() => {
              const notices = [...document.querySelectorAll('[data-sonner-toast]')].filter(notice => !modelName || notice.textContent?.includes(modelName));
              if (timing.completed === null && notices.some(notice => notice.textContent?.includes('Download complete'))) timing.completed = performance.now();
              if (timing.completed !== null && notices.length === 0) { timing.removed = performance.now(); observer.disconnect(); }
            });
            observer.observe(document.body, { childList: true, subtree: true, characterData: true });
            modelsReady = available;
            return timing;
          },
          queries() { return downloadQueries; },
          listenerCount(event: string) { return [...listeners.values()].filter(listener => listener.event === event).length; },
          emit(event: string, payload: unknown) {
            listeners.forEach((listener, id) => {
              if (listener.event === event) (callbacks.get(listener.handler) as Function)?.({ event, id, payload });
            });
          },
        },
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
              case 'plugin:event|emit':
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
              case 'api_get_meetings': return [{ id: 'saved-meeting', title: 'Saved planning meeting' }];
              case 'api_get_meeting_metadata': return {
                id: args.meetingId, title: 'Saved planning meeting',
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
              case 'api_get_model_config': return {
                provider: summaryProvider, model: 'test-model', whisperModel: 'large-v3', apiKey: testSettings.summaryKey,
              };
              case 'api_get_api_key': return args.provider === 'openai' ? testSettings.summaryKey : null;
              case 'api_get_auto_generate_setting': return false;
              case 'api_get_custom_openai_config': return testSettings.customConfig;
              case 'get_custom_openai_models':
                testSettings.modelRequests.push(args);
                if (testSettings.modelDiscoveryError) throw new Error('Server unavailable');
                return ['local-mlx', 'local-gguf'];
              case 'api_save_custom_openai_config': testSettings.customConfig = args as typeof testSettings.customConfig; return null;
              case 'api_save_model_config': testSettings.savedModelConfig = args; return null;
              case 'api_set_auto_generate_setting': return null;
              case 'get_openai_models': return [{ id: 'test-model' }];
              case 'plugin:dialog|message': return testSettings.confirmRemoval ? 'Remove key' : 'Cancel';
              case 'api_delete_api_key':
                testSettings.deletedProviders.push(args.provider);
                if (args.provider === 'openai') testSettings.summaryKey = null;
                return null;
              case 'get_meeting_diarization_status': return null;
              case 'get_recording_preferences': return { preferred_mic_device: null, preferred_system_device: null };
              case 'get_diarization_settings': return { enabled: false, mode: 'off', showProvisionalLabels: false };
              case 'get_diarization_model_status': downloadQueries++; return { installed: modelsReady, downloading: !modelsReady, refinementSupported: false };
              case 'whisper_get_available_models':
              case 'parakeet_get_available_models':
              case 'funasr_get_available_models':
              case 'nemotron_get_available_models': downloadQueries++; return [{
                name: 'notice-test-model', status: modelsReady ? 'Available' : { Downloading: { progress: 99 } },
              }];
              case 'builtin_ai_list_models': downloadQueries++; return [{
                name: 'notice-test-model', status: { type: modelsReady ? 'available' : 'downloading', progress: 99 },
              }];
              case 'get_ollama_download_status': downloadQueries++; return modelsReady ? 'completed' : 'downloading';
              case 'get_recording_state': return { is_recording: false, is_paused: false };
              case 'get_ollama_models':
              case 'get_meeting_speakers':
              case 'list_speaker_profiles':
              case 'api_list_templates': return [];
              default: throw new Error(`Unexpected desktop command: ${command}`);
            }
          },
        },
      });
    }, { format, transcriptText, summaryText: summary, segmentCount, longRowIndex, summaryProvider });

    await serveProductionApp(page);

    await page.goto('https://meetily.test/meeting-details?id=saved-meeting');
    await expect(page.getByRole('region', { name: 'Transcript', exact: true }).getByText(transcriptText, { exact: true })).toBeVisible();
    await expect(page.locator('main .h-screen').last()).toHaveCSS('transform', 'none');
    return errors;
}

test('a saved summary key can be removed from model settings', async ({ page }) => {
  const errors = await openMeeting(page, { summaryProvider: 'openai' });
  const settings = page.getByTitle('Summary Settings');
  await settings.click();
  const dialog = page.getByRole('dialog');
  const key = dialog.getByPlaceholder('Enter your API key');
  const remove = dialog.getByRole('button', { name: 'Remove saved API key' });
  await expect(key).toHaveValue('saved-test-key');
  await remove.click();
  await expect(key).toHaveValue('saved-test-key');

  await page.evaluate(() => {
    (window as any).__MEETILY_TEST_SETTINGS__.confirmRemoval = true;
  });
  await remove.click();
  await expect(key).toHaveValue('');
  await expect(remove).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled();
  expect(await page.evaluate(() => (window as any).__MEETILY_TEST_SETTINGS__.deletedProviders)).toEqual(['openai']);

  await page.keyboard.press('Escape');
  await settings.click();
  const reopened = page.getByRole('dialog');
  await expect(reopened.getByPlaceholder('Enter your API key')).toHaveValue('');
  await expect(reopened.getByRole('button', { name: 'Remove saved API key' })).toHaveCount(0);
  expect(errors).toEqual([]);
});

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


const downloadCases = [
  { provider: 'Parakeet', event: 'parakeet-model-download-progress', payload: { modelName: 'notice-test-model', progress: 99, status: 'downloading' } },
  { provider: 'Whisper', event: 'model-download-progress', payload: { modelName: 'notice-test-model', progress: 99 } },
  { provider: 'FUN-ASR', event: 'funasr-model-download-progress', payload: { modelName: 'notice-test-model', progress: 99, status: 'downloading' } },
  { provider: 'Nemotron', event: 'nemotron-download-progress', payload: { model: 'notice-test-model', percent: 99, phase: 'Install' } },
  { provider: 'Built-in summary', event: 'builtin-ai-download-progress', payload: { model: 'notice-test-model', progress: 99, status: 'downloading' } },
  { provider: 'Ollama', event: 'ollama-model-download-progress', payload: { modelName: 'notice-test-model', progress: 99 } },
  { provider: 'Speaker models', event: 'diarization-model-download-progress', payload: { percent: 99 } },
];
for (const download of downloadCases) {
  test(`${download.provider} notice recovers from a missed completion event`, async ({ page }, info) => {
    const errors = await openMeeting(page);
    await page.waitForFunction(event => (window as any).__downloadTest.listenerCount(event) > 0, download.event);
    await page.evaluate(download => (window as any).__downloadTest.emit(download.event, download.payload), download);
    const notice = page.locator('[data-sonner-toast]').filter({ has: page.getByRole('button', { name: 'Close download notice' }) });
    await expect(notice).toContainText('99%');
    await page.evaluate(() => { (window as any).__noticeTiming = (window as any).__downloadTest.startMeasurement(); });
    await expect(notice).toContainText('Download complete', { timeout: 5000 });
    await expect(notice).toHaveCount(0, { timeout: 6000 });
    const timing = await page.evaluate(() => (window as any).__noticeTiming);
    expect(timing.completed).not.toBeNull();
    expect(timing.removed).not.toBeNull();
    await info.attach('download-notice-latency', { body: JSON.stringify({
      provider: download.provider, browser: info.project.name, kind: 'status-recovery',
      ready_to_complete_ms: timing.completed - timing.started, ready_to_removed_ms: timing.removed - timing.started,
      boundary: 'Production DOM and Sonner; Tauri status replies mocked; no network transfer',
    }), contentType: 'application/json' });
    expect(errors).toEqual([]);
  });
}

test('closing a notice does not reopen it on the next progress update', async ({ page }) => {
  const errors = await openMeeting(page);
  const download = downloadCases[0];
  await page.waitForFunction(event => (window as any).__downloadTest.listenerCount(event) > 0, download.event);
  await page.evaluate(download => (window as any).__downloadTest.emit(download.event, download.payload), download);
  const close = page.getByRole('button', { name: 'Close download notice' });
  await expect(close).toBeVisible();
  await close.focus();
  await expect(close).toBeFocused();
  await page.keyboard.press('Enter');
  await page.evaluate(download => (window as any).__downloadTest.emit(download.event, { ...download.payload, progress: 99 }), download);
  await expect(close).toHaveCount(0);
  expect(errors).toEqual([]);
});

test('a completion in the same commit as another notice closing still updates', async ({ page }) => {
  const errors = await openMeeting(page);
  const download = downloadCases[0];
  await page.waitForFunction(event => (window as any).__downloadTest.listenerCount(event) > 0, download.event);
  for (const modelName of ['closing-notice', 'completing-notice']) {
    await page.evaluate(({ event, modelName }) => (window as any).__downloadTest.emit(event,
      { modelName, progress: 99, status: 'downloading' }), { event: download.event, modelName });
    await expect(page.locator('[data-sonner-toast]').filter({ hasText: modelName })).toContainText('99%');
  }
  // Sonner applies a dismissal two frames later. Emit in those frames so both updates share one commit.
  await page.evaluate(() => {
    const closing = [...document.querySelectorAll('[data-sonner-toast]')].find(notice => notice.textContent?.includes('closing-notice'));
    closing!.querySelector<HTMLButtonElement>('button[aria-label="Close download notice"]')!.click();
    requestAnimationFrame(() => requestAnimationFrame(() => (window as any).__downloadTest.emit(
      'parakeet-model-download-complete', { modelName: 'completing-notice' })));
  });
  await expect(page.locator('[data-sonner-toast]').filter({ hasText: 'completing-notice' })).toContainText('Download complete');
  expect(errors).toEqual([]);
});

test('100% summary progress waits for readiness confirmation', async ({ page }) => {
  const errors = await openMeeting(page);
  const download = downloadCases[4];
  await page.waitForFunction(event => (window as any).__downloadTest.listenerCount(event) > 0, download.event);
  const queries = await page.evaluate(() => (window as any).__downloadTest.queries());
  await page.evaluate(download => (window as any).__downloadTest.emit(download.event, { ...download.payload, progress: 100 }), download);
  await page.waitForFunction(count => (window as any).__downloadTest.queries() > count, queries);
  await expect(page.getByText('Download complete', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Close download notice' })).toBeVisible();
  expect(errors).toEqual([]);
});


test('a completion event updates the notice without waiting for the status poll', async ({ page }, info) => {
  const errors = await openMeeting(page);
  const download = downloadCases[0];
  await page.waitForFunction(event => (window as any).__downloadTest.listenerCount(event) > 0, download.event);
  for (let sample = 0; sample < 20; sample++) {
    const modelName = `notice-event-sample-${sample}`;
    await page.evaluate(({ event, modelName }) => (window as any).__downloadTest.emit(event,
      { modelName, progress: 99, status: 'downloading' }), { event: download.event, modelName });
    const notice = page.locator('[data-sonner-toast]').filter({ hasText: modelName });
    await expect(notice).toContainText('99%');
    await page.evaluate(modelName => {
      (window as any).__noticeTiming = (window as any).__downloadTest.startMeasurement(false, modelName);
      (window as any).__downloadTest.emit('parakeet-model-download-complete', { modelName });
    }, modelName);
    await expect(notice).toContainText('Download complete');
    const timing = await page.evaluate(() => (window as any).__noticeTiming);
    expect(timing.completed).not.toBeNull();
    await info.attach('download-notice-latency', { body: JSON.stringify({
      provider: 'Parakeet', browser: info.project.name, kind: 'completion-event', sample,
      event_to_complete_ms: timing.completed - timing.started,
      boundary: 'Production DOM and Sonner; desktop event injected; no network transfer',
    }), contentType: 'application/json' });
    await notice.getByRole('button', { name: 'Close download notice' }).click();
    await expect(notice).toHaveCount(0);
  }
  expect(errors).toEqual([]);
});


test('LM Studio preset discovers and saves a local summary model', async ({ page }) => {
  const errors = await openMeeting(page, { summaryProvider: 'custom-openai' });
  await page.getByTitle('Summary Settings').click();
  await expect(page.locator('#custom-endpoint')).toHaveValue('http://old-server:8000/v1');
  await page.getByRole('button', { name: 'Use LM Studio', exact: true }).click();
  await expect(page.locator('#custom-endpoint')).toHaveValue('http://localhost:1234/v1');
  await expect(page.locator('#custom-api-key')).toHaveValue('');
  await page.getByRole('button', { name: 'Fetch Models', exact: true }).click();
  await page.locator('#custom-model-list').selectOption('local-mlx');
  await expect(page.locator('#custom-model')).toHaveValue('local-mlx');
  await page.screenshot({ path: test.info().outputPath('lm-studio-settings.png') });
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const saved = await page.evaluate(() => (window as any).__MEETILY_TEST_SETTINGS__);
  expect(saved.modelRequests).toEqual([{ endpoint: 'http://localhost:1234/v1', apiKey: null }]);
  expect(saved.customConfig).toMatchObject({ endpoint: 'http://localhost:1234/v1', model: 'local-mlx', apiKey: null });
  expect(saved.savedModelConfig).toMatchObject({ provider: 'custom-openai', model: 'local-mlx' });
  await page.getByTitle('Summary Settings').click();
  await expect(page.locator('#custom-model')).toHaveValue('local-mlx');
  await page.evaluate(() => { (window as any).__MEETILY_TEST_SETTINGS__.modelDiscoveryError = true; });
  await page.getByRole('button', { name: 'Fetch Models', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Cannot fetch models' })).toBeVisible();
  await expect(page.locator('#custom-model')).toHaveValue('local-mlx');
  await page.locator('#custom-model').fill('manual-model');
  await expect(page.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
  expect(errors.filter(error => !error.includes('Server unavailable'))).toEqual([]);
});
