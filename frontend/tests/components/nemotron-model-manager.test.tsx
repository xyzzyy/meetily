import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const dialogs = { ...await import('@tauri-apps/plugin-dialog') };
const notices = { ...await import('sonner') };
const cards = { ...await import('../../src/components/TranscriptionModelCard') };
let response: (command: string) => unknown | Promise<unknown>;
const invoke = mock((command: string) => Promise.resolve().then(() => response(command)));
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async () => () => {} }));
mock.module('@tauri-apps/plugin-dialog', () => ({ ...dialogs, confirm: async () => true }));
mock.module('sonner', () => ({ ...notices, toast: { success: mock(() => {}), error: mock(() => {}) } }));
let card: any;
mock.module('../../src/components/TranscriptionModelCard', () => ({ ...cards, TranscriptionModelCard: (props: any) => { card = props; return <div />; } }));
const { NemotronModelManager } = await import('../../src/components/NemotronModelManager');
let renderer: ReactTestRenderer | undefined;
const selected = mock(() => {});
const model = { name: 'nemotron-test', display_name: 'Nemotron', supported: true, status: 'Available' };
beforeEach(() => { card = undefined; selected.mockClear(); invoke.mockClear(); response = command => command === 'nemotron_get_available_models' ? [model] : undefined; });
afterEach(async () => { if (renderer) await act(async () => renderer!.unmount()); renderer = undefined; });
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('@tauri-apps/api/event', () => events);
  mock.module('@tauri-apps/plugin-dialog', () => dialogs);
  mock.module('sonner', () => notices);
  mock.module('../../src/components/TranscriptionModelCard', () => cards);
});
async function show() { await act(async () => { renderer = create(<NemotronModelManager autoSave onModelSelect={selected} />); }); }
test('saves the model before reporting a selection, and keeps settings open if save fails', async () => {
  response = command => { if (command === 'api_save_transcript_config') throw new Error('database locked'); return [model]; };
  await show();
  await act(async () => { await card.onSelect(); });
  expect(selected).not.toHaveBeenCalled();
  response = command => command === 'nemotron_get_available_models' ? [model] : undefined;
  await act(async () => { await card.onSelect(); });
  expect(selected).toHaveBeenCalledWith(model.name);
});
test('does not offer a download on unsupported platforms', async () => {
  response = () => [{ ...model, supported: false, status: 'Missing' }];
  await show();
  expect(card).toBeUndefined();
  expect(JSON.stringify(renderer!.toJSON())).toContain('requires macOS on Apple silicon');
});
test('shows download errors and returns the card to a retry state', async () => {
  response = command => { if (command === 'nemotron_download_model') throw new Error('verification failed'); return [{ ...model, status: 'Missing' }]; };
  await show();
  await act(async () => { await card.onDownload(); });
  expect(card.status.kind).toBe('missing');
  expect(JSON.stringify(renderer!.toJSON())).toContain('verification failed');
});
