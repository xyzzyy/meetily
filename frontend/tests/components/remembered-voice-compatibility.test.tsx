import React from 'react';
import { afterAll, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';
const core = { ...await import('@tauri-apps/api/core') };
const navigation = { ...await import('next/navigation') };
const invoke = mock(async (command: string) => {
  if (command !== 'list_speaker_profiles') throw new Error(`Unexpected mutation: ${command}`);
  return [{ id: 'old', name: 'Alice', modelVersion: 'old', recognitionReady: false, sampleCount: 2, speechSeconds: 20, recordingCount: 1 }];
});
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('next/navigation', () => ({ ...navigation, useRouter: () => ({ push() {} }) }));
const { RememberedVoices } = await import('../../src/components/RememberedVoices');
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('next/navigation', () => navigation);
});
test('an old voice remains visible with clear renewal steps and no automatic deletion', async () => {
  let renderer: ReturnType<typeof create>;
  await act(async () => { renderer = create(<RememberedVoices />); });
  try {
    const text = JSON.stringify(renderer!.toJSON());
    expect(text).toContain('Alice');
    expect(text).toContain('Some saved voices need a new sample');
    expect(text).toContain('Existing transcript names are kept.');
    expect(text).toContain('Name only');
    expect(renderer!.root.findAllByProps({ 'aria-label': "Forget Alice's voice" }).length).toBeGreaterThan(0);
    expect(invoke.mock.calls.map(call => call[0])).toEqual(['list_speaker_profiles']);
  } finally { await act(async () => renderer!.unmount()); }
});
