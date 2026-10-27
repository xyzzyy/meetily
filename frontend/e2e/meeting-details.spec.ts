import { expect, test } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import path from 'node:path';

const outputDirectory = path.resolve(__dirname, '../out');
const transcriptText = 'We agreed to review the meeting notes on Friday.';
const summaryText = 'Review the meeting notes. [00:00-00:04]';
const contentTypes: Record<string, string> = {
  '.html': 'text/html', '.txt': 'text/plain', '.js': 'application/javascript',
  '.css': 'text/css', '.woff2': 'font/woff2', '.json': 'application/json',
};

for (const format of ['markdown', 'blocknote', 'empty'] as const) {
  test(`opens a saved meeting with ${format} summary content`, async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    // React reports caught render failures to console.error instead of pageerror.
    page.on('console', message => {
      if (message.type() === 'error') errors.push(message.text());
    });

    // Exercise the real production chunks. Only the desktop IPC boundary is
    // replaced; no React components, editors, or extensions are mocked.
    await page.addInitScript(({ format, transcriptText, summaryText }) => {
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
              case 'check_first_launch': return false;
              case 'plugin:app|version': return '0.4.1';
              case 'api_get_meetings': return [{ id: 'saved-meeting', title: 'Saved planning meeting' }];
              case 'api_get_meeting_metadata': return {
                id: args.meetingId, title: 'Saved planning meeting',
                created_at: '2026-10-27T10:00:00Z', updated_at: '2026-10-27T10:00:00Z',
                folder_path: null,
              };
              case 'api_get_meeting_transcripts': return {
                transcripts: [{
                  id: 'transcript-1', text: transcriptText, timestamp: '10:00:00',
                  audio_start_time: 0, audio_end_time: 4, confidence: 0.98,
                  speaker_label: 'Speaker 1',
                }],
                has_more: false, total_count: 1,
              };
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
    }, { format, transcriptText, summaryText });

    // Serve the static export through request interception, with no dev server.
    await page.route('https://meetily.test/**', async route => {
      const url = new URL(route.request().url());
      let filename = path.join(outputDirectory, decodeURIComponent(url.pathname));
      if (!path.extname(filename)) filename += url.pathname.endsWith('/') ? 'index.html' : '.html';
      const contentType = contentTypes[path.extname(filename)] ?? 'application/octet-stream';
      await route.fulfill({ body: await readFile(filename), contentType });
    });

    await page.goto('https://meetily.test/meeting-details?id=saved-meeting');
    await expect(page.getByText(transcriptText, { exact: true })).toBeVisible();
    if (format === 'empty') {
      await expect(page.getByText('No Summary Generated Yet', { exact: true })).toBeVisible();
    } else {
      const editor = page.locator('.bn-editor');
      await expect(editor).toBeVisible();
      await expect(editor).toContainText(summaryText);
      await expect(editor).toHaveAttribute('contenteditable', 'true');
      await editor.locator('.transcript-ref').click();
      await expect(page.locator('#segment-transcript-1')).toHaveClass(/ring-2/);
      await editor.click();
      await page.keyboard.press('End');
      await page.keyboard.type(' Updated');
      await expect(editor).toContainText('Updated');
    }
    await expect(page.getByText('Application error:', { exact: false })).toHaveCount(0);
    expect(errors).toEqual([]);
  });
}
