import { expect, test, type Page } from '@playwright/test';
import { collectAppErrors, serveProductionApp } from './support/production-app';

const transcriptText = 'We agreed to review the meeting notes on Friday.';
const summaryText = 'Review the meeting notes. [00:00-00:04]';

async function openMeeting(page: Page, {
  format = 'markdown', segmentCount = 1, summary = summaryText, longRowIndex,
}: {
  format?: 'markdown' | 'blocknote' | 'empty';
  segmentCount?: number;
  summary?: string;
  longRowIndex?: number;
} = {}) {
    const errors = collectAppErrors(page);

    // Exercise the real production chunks. Only the desktop IPC boundary is
    // replaced; no React components, editors, or extensions are mocked.
    await page.addInitScript(({ format, transcriptText, summaryText, segmentCount, longRowIndex }) => {
      let callbackId = 0;
      const callbacks = new Map<number, unknown>();
      const store = new Map<string, unknown>();
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
              case 'api_get_model_config': return { provider: 'ollama', model: 'test-model', whisperModel: 'large-v3' };
              case 'api_get_api_key':
              case 'get_meeting_diarization_status': return null;
              case 'get_recording_preferences': return { preferred_mic_device: null, preferred_system_device: null };
              case 'get_diarization_settings': return { enabled: false, mode: 'off', showProvisionalLabels: false };
              case 'get_diarization_model_status': return { installed: false, refinementSupported: false };
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
    }, { format, transcriptText, summaryText: summary, segmentCount, longRowIndex });

    await serveProductionApp(page);

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
