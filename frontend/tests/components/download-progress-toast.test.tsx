import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, spyOn, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const notices = { ...await import('sonner') };
const handlers = new Map<string, (event: any) => void>();
let responses: Record<string, unknown>;
const invoke = mock(async (command: string, _args?: unknown): Promise<any> => responses[command]);
const custom = mock((_content: any, _options: any) => 'toast');
const dismiss = mock((_id: string) => {});
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => handlers.delete(name);
} }));
mock.module('sonner', () => ({ ...notices, toast: { custom, dismiss } }));
const { DownloadProgressToastProvider } = await import('../../src/components/shared/DownloadProgressToast');
const { downloadProviders } = await import('../../src/lib/model-downloads');
let renderer: ReactTestRenderer;
let checkStatus: () => Promise<void>;
let cleanups: Map<number, () => void>;
const originalTimeout = globalThis.setTimeout;
let intervalSpy: ReturnType<typeof spyOn>;
let timeoutSpy: ReturnType<typeof spyOn>;
const current = () => custom.mock.calls.at(-1)![0]('toast');
const send = async (event: string, payload: any) => { await act(async () => handlers.get(event)!({ payload })); };

beforeEach(async () => {
  responses = {}; cleanups = new Map();
  handlers.clear(); invoke.mockClear(); custom.mockClear(); dismiss.mockClear();
  intervalSpy = spyOn(globalThis, 'setInterval').mockImplementation(((callback: any) => {
    checkStatus = callback;
    return 123;
  }) as any);
  timeoutSpy = spyOn(globalThis, 'setTimeout').mockImplementation(((callback: any, delay: number, ...args: any[]) => {
    if ([4000, 6000, 11000].includes(delay)) { cleanups.set(delay, callback); return delay; }
    return originalTimeout(callback, delay, ...args);
  }) as any);
  await act(async () => { renderer = create(<DownloadProgressToastProvider />); });
});
afterEach(async () => {
  await act(async () => renderer.unmount());
  intervalSpy.mockRestore(); timeoutSpy.mockRestore();
});
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('@tauri-apps/api/event', () => events);
  mock.module('sonner', () => notices);
});

for (const provider of downloadProviders) {
  const model = provider.fixedModel ?? 'test-model';
  const progress = { modelName: model, model, progress: 100, percent: 100, downloaded_mb: 2431.8, total_mb: 2431.8, status: 'downloading' };
  const id = `download-${provider.id}:${model}`;
  test(`${provider.id}: verifies completion after a lost event, then removes the notice`, async () => {
    responses[provider.list] = provider.id === 'ollama' ? 'completed' : provider.id === 'diarization'
      ? { installed: true } : [{ name: model, status: provider.id === 'builtin' ? { type: 'available' } : 'Available' }];
    await send(provider.progress, progress);
    expect(current().props.download.status).toBe('downloading');
    await act(async () => checkStatus());
    expect(current().props.download.status).toBe('completed');
    expect(current().props.download.progress).toBe(100);
    await act(async () => cleanups.get(4000)!());
    expect(dismiss).toHaveBeenCalledWith(id);
  });
  test(`${provider.id}: 100% does not complete a model still being installed`, async () => {
    responses[provider.list] = provider.id === 'ollama' ? 'downloading' : provider.id === 'diarization'
      ? { installed: false } : [{ name: model, status: provider.id === 'builtin' ? { type: 'downloading' } : { Downloading: { progress: 100 } } }];
    await send(provider.progress, progress);
    await act(async () => checkStatus());
    expect(current().props.download.status).toBe('downloading');
    expect(dismiss).not.toHaveBeenCalled();
  });
  test(`${provider.id}: uses the same completion and size-preserving cleanup`, async () => {
    await send(provider.progress, progress);
    await send(provider.complete ?? provider.finished ?? provider.progress, { modelName: model, model, status: 'completed', total_mb: 0, downloaded_mb: 0 });
    expect(current().props.download.status).toBe('completed');
    expect(current().props.download.totalMb).toBe(2431.8);
    await act(async () => cleanups.get(4000)!());
    expect(dismiss).toHaveBeenCalledWith(id);
  });
  test(`${provider.id}: uses the same error cleanup`, async () => {
    await send(provider.progress, progress);
    await send(provider.error ?? provider.finished ?? provider.progress, { modelName: model, model, error: 'Connection failed' });
    expect(current().props.download.status).toBe('error');
    await act(async () => cleanups.get(11000)!());
    expect(dismiss).toHaveBeenCalledWith(id);
  });
  test(`${provider.id}: can close a notice without later progress reopening it`, async () => {
    await send(provider.progress, progress);
    let content: ReactTestRenderer;
    await act(async () => { content = create(current()); });
    await act(async () => content!.root.findByProps({ 'aria-label': 'Close download notice' }).props.onClick());
    expect(dismiss).toHaveBeenCalledWith(id);
    const count = custom.mock.calls.length;
    await send(provider.progress, progress);
    expect(custom.mock.calls.length).toBe(count);
    await act(async () => content!.unmount());
  });
}

test('providers with the same model name do not overwrite each other', async () => {
  await send('model-download-progress', { modelName: 'same', progress: 40 });
  await send('ollama-model-download-progress', { modelName: 'same', progress: 50 });
  const ids = new Set(custom.mock.calls.map(call => call[1].id));
  expect(ids.has('download-whisper:same')).toBe(true);
  expect(ids.has('download-ollama:same')).toBe(true);
});

test('late progress does not replace a terminal notice', async () => {
  await send('parakeet-model-download-complete', { modelName: 'model' });
  await send('parakeet-model-download-progress', { modelName: 'model', progress: 99 });
  expect(current().props.download.status).toBe('completed');
});

test('Ollama checks the server that received the download request', async () => {
  responses.get_ollama_download_status = 'downloading';
  await send('ollama-model-download-progress', { modelName: 'model', progress: 99, endpoint: 'http://remote.test:11434' });
  await act(async () => checkStatus());
  expect(invoke).toHaveBeenCalledWith('get_ollama_download_status', { modelName: 'model', endpoint: 'http://remote.test:11434' });
});

test('a slow provider does not block status recovery for another provider', async () => {
  responses.parakeet_get_available_models = new Promise(() => {});
  responses.whisper_get_available_models = [{ name: 'small', status: 'Available' }];
  await send('parakeet-model-download-progress', { modelName: 'parakeet', progress: 99 });
  await send('model-download-progress', { modelName: 'small', progress: 99 });
  await act(async () => { void checkStatus(); await Promise.resolve(); await Promise.resolve(); });
  const content = custom.mock.calls.map(call => call[0]('toast').props.download);
  expect(content.some(download => download.provider === 'whisper' && download.status === 'completed')).toBe(true);
});

test('a status reply from an older attempt cannot complete a retry', async () => {
  let resolve!: (value: unknown) => void;
  responses.parakeet_get_available_models = new Promise(done => { resolve = done; });
  await send('parakeet-model-download-progress', { modelName: 'model', progress: 99 });
  const checking = checkStatus();
  await send('parakeet-model-download-complete', { modelName: 'model' });
  await send('parakeet-model-download-progress', { modelName: 'model', progress: 0 });
  await act(async () => { resolve([{ name: 'model', status: 'Available' }]); await checking; });
  expect(current().props.download.status).toBe('downloading');
});

test('worker-confirmed cancellation uses the common cancellation cleanup', async () => {
  const cases = [
    ['model-download-cancelled', { modelName: 'whisper' }],
    ['parakeet-model-download-progress', { modelName: 'parakeet', status: 'cancelled' }],
    ['funasr-model-download-error', { modelName: 'funasr', status: 'cancelled', error: 'Download cancelled' }],
    ['nemotron-download-finished', { model: 'nemotron', error: 'Download cancelled.' }],
    ['builtin-ai-download-progress', { model: 'summary', status: 'cancelled' }],
  ] as const;
  for (const [event, payload] of cases) {
    await send(event, payload);
    expect(current().props.download.status).toBe('cancelled');
    expect(cleanups.has(6000)).toBe(true);
  }
});
