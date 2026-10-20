import { afterAll, afterEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { DEFAULT_DIARIZATION_SETTINGS } from '../../src/types/diarization';

const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvent = { ...await import('@tauri-apps/api/event') };
const originalConfig = { ...await import('../../src/contexts/ConfigContext') };
const originalVoices = { ...await import('../../src/components/RememberedVoices') };
let status: Promise<{ installed: boolean; refinementSupported: boolean }>;
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke: () => status }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvent, listen: async () => () => {} }));
mock.module('../../src/contexts/ConfigContext', () => ({ useConfig: () => ({
  diarizationSettings: { ...DEFAULT_DIARIZATION_SETTINGS, enabled: true },
  isLoadingDiarizationSettings: false, updateDiarizationSettings: async () => {},
}) }));
mock.module('../../src/components/RememberedVoices', () => ({ RememberedVoices: () => null }));
const { SpeakerDiarizationSettings } = await import('../../src/components/SpeakerDiarizationSettings');
const { SettingsRow } = await import('../../src/components/settings/SettingsRow');
let renderer: ReactTestRenderer;
afterEach(() => { act(() => renderer?.unmount()); });
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('../../src/contexts/ConfigContext', () => originalConfig);
  mock.module('../../src/components/RememberedVoices', () => originalVoices);
  mock.restore();
});
const row = (title: string) => renderer.root.findAllByType(SettingsRow).find(node => node.props.title === title)!;

for (const supported of [false, true]) {
  test(`post-call controls follow platform support (${supported}) without blocking live labels`, async () => {
    status = Promise.resolve({ installed: true, refinementSupported: supported });
    await act(async () => { renderer = create(<SpeakerDiarizationSettings />); });
    expect(row('Run post-call refinement automatically').props.control.props.disabled).toBe(!supported);
    expect(row('Run post-call refinement automatically').props.control.props.checked).toBe(supported);
    expect(row('Show speaker labels during recording').props.control.props.disabled).toBe(false);
    expect(row('Enable speaker labels').props.control.props.disabled).toBe(false);
    if (!supported) expect(row('Run post-call refinement automatically').props.description).toContain('Apple Silicon');
  });
}

test('post-call controls stay disabled until the check completes', async () => {
  let resolve!: (value: { installed: boolean; refinementSupported: boolean }) => void;
  status = new Promise(yes => { resolve = yes; });
  await act(async () => { renderer = create(<SpeakerDiarizationSettings />); });
  expect(row('Run post-call refinement automatically').props.control.props.disabled).toBe(true);
  await act(async () => { resolve({ installed: true, refinementSupported: true }); });
  expect(row('Run post-call refinement automatically').props.control.props.disabled).toBe(false);
});

test('a failed capability check explains why post-call analysis is unavailable', async () => {
  status = Promise.reject(new Error('test status failure'));
  await act(async () => { renderer = create(<SpeakerDiarizationSettings />); });
  expect(row('Run post-call refinement automatically').props.control.props.disabled).toBe(true);
  expect(row('Run post-call refinement automatically').props.description).toContain('Could not check');
});
