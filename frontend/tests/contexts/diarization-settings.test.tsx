import { afterAll, afterEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { DEFAULT_DIARIZATION_SETTINGS, type DiarizationSettings } from '../../src/types/diarization';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalService = { ...await import('../../src/services/configService') };
let save: (settings: DiarizationSettings) => Promise<void>;
const saved = { ...DEFAULT_DIARIZATION_SETTINGS, showProvisionalLabels: false };
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke: async () => [] }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen: async () => () => {} }));
mock.module('../../src/services/configService', () => ({
  ...originalService,
  configService: {
    getTranscriptConfig: async () => null,
    getModelConfig: async () => null,
    getRecordingPreferences: async () => null,
    getDiarizationSettings: async () => saved,
    saveDiarizationSettings: (settings: DiarizationSettings) => save(settings),
  },
}));
const { ConfigProvider, useConfig } = await import('../../src/contexts/ConfigContext');
let current: ReturnType<typeof useConfig>;
function ReadConfig() { current = useConfig(); return null; }
let renderer: ReactTestRenderer;
afterEach(() => { act(() => renderer?.unmount()); });
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('../../src/services/configService', () => originalService);
});
function deferred() {
  let resolve!: () => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function mount() {
  await act(async () => { renderer = create(<ConfigProvider><ReadConfig /></ConfigProvider>); });
  expect(current.diarizationSettings).toEqual(saved);
}

// Exercise the real provider with delayed storage to reproduce overlapping saves.
describe('speaker settings persistence', () => {
  for (const firstSucceeds of [false, true]) {
    test(`latest failure restores ${firstSucceeds ? 'the first saved edit' : 'the loaded settings'}`, async () => {
      const first = deferred();
      const second = deferred();
      const calls: DiarizationSettings[] = [];
      save = settings => { calls.push(settings); return calls.length === 1 ? first.promise : second.promise; };
      await mount();
      const edit1 = { ...saved, enabled: true };
      const edit2 = { ...edit1, showProvisionalLabels: true };
      let result1!: Promise<unknown>;
      let result2!: Promise<unknown>;
      await act(async () => {
        result1 = current.updateDiarizationSettings(edit1).catch(error => error);
        result2 = current.updateDiarizationSettings(edit2).catch(error => error);
      });
      expect(calls).toEqual([edit1]);
      expect(current.diarizationSettings).toEqual(edit2);
      await act(async () => {
        if (firstSucceeds) first.resolve(); else first.reject(new Error('first failed'));
        await result1;
      });
      expect(calls).toEqual([edit1, edit2]);
      expect(current.diarizationSettings).toEqual(edit2);
      await act(async () => { second.reject(new Error('second failed')); await result2; });
      expect(current.diarizationSettings).toEqual(firstSucceeds ? edit1 : saved);
    });
  }

  test('a queued save can succeed after an earlier failure', async () => {
    const first = deferred();
    let count = 0;
    save = () => ++count === 1 ? first.promise : Promise.resolve();
    await mount();
    const latest = { ...saved, enabled: true, showProvisionalLabels: true };
    await act(async () => {
      const failed = current.updateDiarizationSettings({ ...saved, enabled: true }).catch(error => error);
      const success = current.updateDiarizationSettings(latest);
      first.reject(new Error('first failed'));
      await Promise.all([failed, success]);
    });
    expect(current.diarizationSettings).toEqual(latest);
  });
});
