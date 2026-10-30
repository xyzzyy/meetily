import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, spyOn, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const notices = { ...await import('sonner') };
const handlers = new Map<string, (event: any) => void>();
const invoke = mock(async (_command: string) => models);
const custom = mock((_content: any, _options: any) => 'toast');
const dismiss = mock((_id: string) => {});
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => handlers.delete(name);
} }));
mock.module('sonner', () => ({ ...notices, toast: { custom, dismiss } }));
const { DownloadProgressToastProvider } = await import('../../src/components/shared/DownloadProgressToast');
let models: any[];
let renderer: ReactTestRenderer;
let checkStatus: () => Promise<void>;
let cleanup: () => void;
const originalTimeout = globalThis.setTimeout;
let intervalSpy: ReturnType<typeof spyOn>;
let timeoutSpy: ReturnType<typeof spyOn>;
const name = 'parakeet-tdt-0.6b-v3-fp32';
const progress = { modelName: name, progress: 99, downloaded_mb: 2431.8, total_mb: 2431.8, status: 'downloading' };
const current = () => custom.mock.calls.at(-1)![0]('toast');

beforeEach(async () => {
  models = [{ name, status: 'Available' }];
  handlers.clear(); invoke.mockClear(); custom.mockClear(); dismiss.mockClear();
  intervalSpy = spyOn(globalThis, 'setInterval').mockImplementation(((callback: any) => {
    checkStatus = callback;
    return 123;
  }) as any);
  timeoutSpy = spyOn(globalThis, 'setTimeout').mockImplementation(((callback: any, delay: number, ...args: any[]) => {
    if (delay === 4000) { cleanup = callback; return 456; }
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

test('clears a 99% notice when the engine confirms completion without an event', async () => {
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: progress }));
  expect(current().props.download.status).toBe('downloading');
  await act(async () => checkStatus());
  expect(invoke).toHaveBeenCalledWith('parakeet_get_available_models');
  expect(current().props.download.status).toBe('completed');
  expect(current().props.download.progress).toBe(100);
  await act(async () => cleanup());
  expect(dismiss).toHaveBeenCalledWith(`download-${name}`);
});

test('equal byte counts do not complete a download while the engine is still downloading', async () => {
  models = [{ name, status: { Downloading: { progress: 99 } } }];
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: progress }));
  await act(async () => checkStatus());
  expect(current().props.download.status).toBe('downloading');
  expect(dismiss).not.toHaveBeenCalled();
});

test('the close button dismisses the notice and keeps later progress hidden', async () => {
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: progress }));
  let content: ReactTestRenderer;
  await act(async () => { content = create(current()); });
  await act(async () => content!.root.findByProps({ 'aria-label': 'Close download notice' }).props.onClick());
  expect(dismiss).toHaveBeenCalledWith(`download-${name}`);
  const count = custom.mock.calls.length;
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: progress }));
  expect(custom.mock.calls.length).toBe(count);
  await act(async () => content!.unmount());
});

test('completion in a progress event also removes the persistent notice', async () => {
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: {
    ...progress, progress: 100, status: 'completed',
  } }));
  expect(current().props.download.status).toBe('completed');
  await act(async () => cleanup());
  expect(dismiss).toHaveBeenCalledWith(`download-${name}`);
});

test('a completion event retains the full precision model size', async () => {
  await act(async () => handlers.get('parakeet-model-download-progress')!({ payload: progress }));
  await act(async () => handlers.get('parakeet-model-download-complete')!({ payload: { modelName: name } }));
  expect(current().props.download.totalMb).toBe(2431.8);
  await act(async () => cleanup());
});
