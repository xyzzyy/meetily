import React from 'react';
import { afterEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const backends = ['coreaudio', 'test-backend'].map(id => ({ id, name: id, description: id }));
let available = backends;
const invoke = mock(async (command: string) => command === 'get_audio_backend_info' ? available : 'coreaudio');
mock.module('@tauri-apps/api/core', () => ({ invoke }));
const { AudioBackendSelector } = await import('../../src/components/AudioBackendSelector');
let view: ReactTestRenderer;
afterEach(() => { view?.unmount(); invoke.mockClear(); available = backends; });

test('hides the picker when macOS lists only Core Audio', async () => {
  available = [backends[0]];
  await act(async () => { view = create(<AudioBackendSelector />); });
  expect(view.toJSON()).toBeNull();
});

test('both listed backends can be selected and the selection is saved', async () => {
  const changed = mock(() => {});
  await act(async () => { view = create(<AudioBackendSelector onBackendChange={changed} />); });
  const radios = () => view.root.findAllByType('input');
  expect(radios().map(r => r.props.disabled)).toEqual([false, false]);
  await act(async () => { await radios()[1].props.onChange(); });
  expect(invoke).toHaveBeenCalledWith('set_audio_backend', { backend: 'test-backend' });
  expect(changed).toHaveBeenCalledWith('test-backend');
  expect(radios()[1].props.checked).toBe(true);
  await act(async () => { await radios()[0].props.onChange(); });
  expect(invoke).toHaveBeenCalledWith('set_audio_backend', { backend: 'coreaudio' });
  expect(radios()[0].props.checked).toBe(true);
});

test('the parent can disable all backend controls', async () => {
  await act(async () => { view = create(<AudioBackendSelector disabled />); });
  expect(view.root.findAllByType('input').map(r => r.props.disabled)).toEqual([true, true]);
});
