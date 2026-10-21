import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
let respond: (command: string) => unknown | Promise<unknown>;
const invoke = mock((command: string) => Promise.resolve().then(() => respond(command)));
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke }));
const { useTranscriptionModels } = await import('../../src/hooks/useTranscriptionModels');
afterAll(() => mock.module('@tauri-apps/api/core', () => originalCore));
let renderer: ReactTestRenderer | undefined;
let state: ReturnType<typeof useTranscriptionModels>;
function View({ config }: { config?: { provider: string; model: string } }) {
  state = useTranscriptionModels(config);
  return null;
}
const models = [{ name: 'test', size_mb: 1, status: 'Available' }];
beforeEach(() => {
  invoke.mockClear();
  respond = command => command.endsWith('_init') ? undefined : models;
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});
async function show(config?: { provider: string; model: string }) {
  await act(async () => { renderer = create(<View config={config} />); });
}
test('waits for FUN-ASR initialization before listing its models', async () => {
  let initialized = false;
  let finish: () => void = () => {};
  const ready = new Promise<void>(resolve => { finish = () => { initialized = true; resolve(); }; });
  respond = command => {
    if (command === 'funasr_init') return ready;
    if (command === 'funasr_get_available_models') expect(initialized).toBe(true);
    return command.endsWith('_init') ? undefined : models;
  };
  await show({ provider: 'funasr', model: 'test' });
  let fetch: Promise<void>;
  await act(async () => { fetch = state.fetchModels(); });
  expect(invoke.mock.calls.some(([command]) => command === 'funasr_get_available_models')).toBe(false);
  await act(async () => { finish(); await fetch!; });
  expect(state.selectedModelKey).toBe('funasr:test');
});
test('shows initialization errors without selecting a different provider', async () => {
  respond = command => {
    if (command === 'funasr_init') throw new Error('model directory unavailable');
    return command.endsWith('_init') ? undefined : models;
  };
  await show({ provider: 'funasr', model: 'test' });
  await act(async () => { await state.fetchModels(); });
  expect(state.availableModels.length).toBe(2);
  expect(state.selectedModelKey).toBe('');
  expect(state.modelNotice).toContain('model directory unavailable');
});
test('requires a choice when no config exists or the saved model is missing', async () => {
  await show();
  await act(async () => { await state.fetchModels(); });
  expect(state.selectedModelKey).toBe('');
  expect(state.modelNotice).toContain('Select a transcription model');
  await act(async () => { renderer!.update(<View config={{ provider: 'funasr', model: 'missing' }} />); });
  expect(state.selectedModelKey).toBe('');
  expect(state.modelNotice).toContain('missing');
});
test('applies settings loaded after the dialog opens, but keeps a manual choice', async () => {
  await show();
  await act(async () => { await state.fetchModels(); });
  await act(async () => { renderer!.update(<View config={{ provider: 'funasr', model: 'test' }} />); });
  expect(state.selectedModelKey).toBe('funasr:test');
  await act(async () => { state.setSelectedModelKey('parakeet:test'); });
  await act(async () => { await state.fetchModels(); });
  expect(state.selectedModelKey).toBe('parakeet:test');
});
