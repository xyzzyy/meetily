import React from 'react';
import { afterAll, afterEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalService = { ...await import('../../src/services/configService') };
const providers = ['claude', 'groq', 'openai', 'openrouter'] as const;
type Provider = typeof providers[number];

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(yes => { resolve = yes; });
  return { promise, resolve };
}
let requests: Record<Provider, ReturnType<typeof deferred<string>>>;
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke: async (command: string, args?: { provider?: Provider }) => {
  if (command === 'api_get_api_key' && args?.provider) return requests[args.provider].promise;
  if (command === 'get_ollama_models') return [];
  return null;
} }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen: async () => () => {} }));
mock.module('../../src/services/configService', () => ({
  ...originalService,
  configService: {
    getTranscriptConfig: async () => null,
    getModelConfig: async () => null,
    getRecordingPreferences: async () => null,
    getDiarizationSettings: async () => null,
  },
}));
const { ConfigProvider, useConfig } = await import('../../src/contexts/ConfigContext');
let current: ReturnType<typeof useConfig>;
let renderer: ReactTestRenderer;
function ReadConfig() { current = useConfig(); return null; }
afterEach(() => { act(() => renderer?.unmount()); });
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('../../src/services/configService', () => originalService);
});

for (const newKey of [null, 'replacement-test-key']) {
  test(`late startup reads do not restore a ${newKey === null ? 'removed' : 'replaced'} key`, async () => {
    requests = Object.fromEntries(providers.map(provider => [provider, deferred<string>()])) as typeof requests;
    await act(async () => { renderer = create(<ConfigProvider><ReadConfig /></ConfigProvider>); });
    await act(async () => { current.updateProviderApiKey('openai', newKey); });
    await act(async () => {
      for (const provider of providers) requests[provider].resolve(`${provider}-old-key`);
      await Promise.all(providers.map(provider => requests[provider].promise));
    });
    expect(current.providerApiKeys.openai).toBe(newKey);
    expect(current.providerApiKeys.claude).toBe('claude-old-key');
    expect(current.providerApiKeys.groq).toBe('groq-old-key');
    expect(current.providerApiKeys.openrouter).toBe('openrouter-old-key');
  });
}

test('model settings operations remain exclusive and release the lock after failure', async () => {
  requests = Object.fromEntries(providers.map(provider => [provider, deferred<string>()])) as typeof requests;
  await act(async () => { renderer = create(<ConfigProvider><ReadConfig /></ConfigProvider>); });
  const pending = deferred<void>();
  let first!: Promise<boolean>;
  let secondRan = false;
  await act(async () => { first = current.runModelSettingsOperation(async () => pending.promise); });
  expect(current.isModelSettingsBusy).toBe(true);
  let second!: boolean;
  await act(async () => { second = await current.runModelSettingsOperation(async () => { secondRan = true; }); });
  expect(second).toBe(false);
  expect(secondRan).toBe(false);
  await act(async () => { pending.resolve(); expect(await first).toBe(true); });
  expect(current.isModelSettingsBusy).toBe(false);
  await act(async () => {
    await expect(current.runModelSettingsOperation(async () => { throw new Error('save failed'); })).rejects.toThrow('save failed');
  });
  expect(current.isModelSettingsBusy).toBe(false);
  await act(async () => { expect(await current.runModelSettingsOperation(async () => {})).toBe(true); });
  await act(async () => {
    for (const provider of providers) requests[provider].resolve('');
    await Promise.all(providers.map(provider => requests[provider].promise));
  });
});
