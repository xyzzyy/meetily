import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { FunAsrModelInfo } from '../../src/lib/funasr';

const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const dialogs = { ...await import('@tauri-apps/plugin-dialog') };
const notifications = { ...await import('sonner') };
const buttonModule = { ...await import('../../src/components/ui/button') };
const progressModule = { ...await import('../../src/components/ui/progress') };
let reply: (command: string, args?: any) => unknown | Promise<unknown>;
const invoke = mock((command: string, args?: any) => Promise.resolve().then(() => reply(command, args)));
const errorToast = mock(() => {});
const handlers = new Map<string, (event: any) => void | Promise<void>>();
const confirm = mock(async (_message: string, _options?: unknown) => true);
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => handlers.delete(name);
} }));
mock.module('@tauri-apps/plugin-dialog', () => ({ ...dialogs, confirm }));
mock.module('sonner', () => ({ ...notifications, toast: { error: errorToast, success: mock(() => {}) } }));
mock.module('../../src/components/ui/button', () => ({ ...buttonModule,
  Button: ({ children, ...props }: any) => <button {...props}>{children}</button>,
}));
mock.module('../../src/components/ui/progress', () => ({ ...progressModule,
  Progress: ({ value }: any) => <progress value={value} />,
}));
const { FunAsrModelManager } = await import('../../src/components/FunAsrModelManager');
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('@tauri-apps/api/event', () => events);
  mock.module('@tauri-apps/plugin-dialog', () => dialogs);
  mock.module('sonner', () => notifications);
  mock.module('../../src/components/ui/button', () => buttonModule);
  mock.module('../../src/components/ui/progress', () => progressModule);
});
const model: FunAsrModelInfo = {
  name: 'nano-q4', display_name: 'Nano Q4', path: '/fixture/model',
  encoder_available: true, has_partial_download: false, has_orphaned_encoder: false, size_mb: 950,
  decoder_size_mb: 484, encoder_size_mb: 469, description: 'Test model',
  recommended: false, status: 'Available',
};
let available: FunAsrModelInfo[];
let renderer: ReactTestRenderer | undefined;
const selected = mock((_name: string) => {});
beforeEach(() => {
  available = [{ ...model }];
  invoke.mockClear(); errorToast.mockClear(); selected.mockClear(); handlers.clear(); confirm.mockClear();
  reply = command => command === 'funasr_get_available_models' ? available
    : command === 'funasr_cancel_download' ? true : undefined;
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});
async function show() {
  await act(async () => { renderer = create(<FunAsrModelManager selectedModel="previous" onModelSelect={selected} autoSave />); });
}
function selectButton() {
  return renderer!.root.findAllByType('button').find(button => button.props.className?.includes('min-w-0'))!;
}
async function click(title: string) {
  const button = renderer!.root.findAllByType('button').find(button => button.props.title === title)!;
  expect(button).toBeDefined();
  await act(async () => { await button.props.onClick(); });
}
test('does not change the selection or close settings when saving fails', async () => {
  reply = command => {
    if (command === 'api_save_transcript_config') throw new Error('database is locked');
    return command === 'funasr_get_available_models' ? available : undefined;
  };
  await show();
  await act(async () => { await selectButton().props.onClick(); });
  expect(selected).not.toHaveBeenCalled();
  expect(errorToast.mock.calls.length).toBe(1);
  expect(selectButton().props.disabled).toBe(false);
});
test('updates the selection only after saving succeeds and blocks competing selections', async () => {
  let finish: () => void = () => {};
  const saved = new Promise<void>(resolve => { finish = resolve; });
  reply = command => command === 'api_save_transcript_config' ? saved
    : command === 'funasr_get_available_models' ? available : undefined;
  await show();
  let saving: Promise<void>;
  await act(async () => { saving = selectButton().props.onClick(); });
  expect(selected).not.toHaveBeenCalled();
  expect(selectButton().props.disabled).toBe(true);
  await act(async () => { finish(); await saving!; });
  expect(selected).toHaveBeenCalledWith('nano-q4');
});
test('shows Cancel and saved progress when reopened during a download', async () => {
  available = [{ ...model, status: { Downloading: { progress: 42 } } }];
  await show();
  expect(renderer!.root.findByType('progress').props.value).toBe(42);
  await click('Cancel download');
  expect(invoke).toHaveBeenCalledWith('funasr_cancel_download', { modelName: 'nano-q4' });
});
test('shows a command error and clears the optimistic download state without an event', async () => {
  available = [{ ...model, status: 'Missing' }];
  reply = command => {
    if (command === 'funasr_download_model') throw new Error('engine unavailable');
    return command === 'funasr_get_available_models' ? available : undefined;
  };
  await show();
  const download = renderer!.root.findAllByType('button').find(button => button.props.children?.[1] === 'Download')!;
  await act(async () => { await download.props.onClick(); });
  expect(errorToast.mock.calls.length).toBe(1);
  expect(renderer!.root.findAllByType('progress')).toHaveLength(0);
  expect(renderer!.root.findAllByType('button').some(button => button.props.title === 'Cancel download')).toBe(false);
});
test('provides deletion for a partial download', async () => {
  available = [{ ...model, status: 'Missing', has_partial_download: true }];
  await show();
  await click('Delete partial download');
  expect(confirm).toHaveBeenCalled();
  expect(invoke).toHaveBeenCalledWith('funasr_delete_model', { modelName: 'nano-q4' });
});
test('finishing a download does not change the saved model without a selection', async () => {
  await show();
  await act(async () => { await handlers.get('funasr-model-download-complete')!({ payload: { modelName: 'nano-q4' } }); });
  expect(selected).not.toHaveBeenCalled();
  expect(invoke.mock.calls.some(([command]) => command === 'api_save_transcript_config')).toBe(false);
});

test('offers cleanup for a completed encoder with no decoder or partial file', async () => {
  available = [{ ...model, status: 'Missing', has_orphaned_encoder: true }];
  reply = command => {
    if (command === 'funasr_delete_model') available = [{ ...model, status: 'Missing', encoder_available: false }];
    return command === 'funasr_get_available_models' ? available : undefined;
  };
  await show();
  expect(renderer!.root.findAllByProps({ title: 'Delete partial download' })).toHaveLength(0);
  await click('Delete unused encoder');
  expect(invoke).toHaveBeenCalledWith('funasr_delete_model', { modelName: 'nano-q4' });
  expect(confirm.mock.calls[0]?.[0]).toContain('unused shared FUN-ASR encoder');
  expect(renderer!.root.findAllByProps({ title: 'Delete unused encoder' })).toHaveLength(0);
});

test('a cancellation rejection without an event is not shown as a failure', async () => {
  available = [{ ...model, status: 'Missing' }];
  reply = command => {
    if (command === 'funasr_download_model') {
      available = [{ ...model, status: 'Missing', has_partial_download: true }];
      return Promise.reject('FUN-ASR model download cancelled');
    }
    return command === 'funasr_get_available_models' ? available : undefined;
  };
  await show();
  const download = renderer!.root.findAllByType('button').find(button => button.props.children?.[1] === 'Download')!;
  await act(async () => { await download.props.onClick(); });
  expect(errorToast).not.toHaveBeenCalled();
  expect(renderer!.root.findAllByType('progress')).toHaveLength(0);
  expect(renderer!.root.findAllByProps({ title: 'Delete partial download' }).length).toBeGreaterThan(0);
});
for (const eventFirst of [true, false]) {
  test(`a cancelled event and rejection do not show a failure (event first: ${eventFirst})`, async () => {
    available = [{ ...model, status: 'Missing' }];
    let rejectDownload: (error: unknown) => void = () => {};
    const pending = new Promise<void>((_resolve, reject) => { rejectDownload = reject; });
    reply = command => command === 'funasr_download_model' ? pending
      : command === 'funasr_get_available_models' ? available : undefined;
    await show();
    const button = renderer!.root.findAllByType('button').find(button => button.props.children?.[1] === 'Download')!;
    let download: Promise<void>;
    await act(async () => { download = button.props.onClick(); });
    available = [{ ...model, status: 'Missing', has_partial_download: true }];
    const event = () => handlers.get('funasr-model-download-error')!({ payload: {
      modelName: model.name, error: 'FUN-ASR model download cancelled', status: 'cancelled',
    } });
    await act(async () => {
      if (eventFirst) event();
      rejectDownload(new Error('FUN-ASR model download cancelled'));
      await download!;
      if (!eventFirst) event();
    });
    expect(errorToast).not.toHaveBeenCalled();
    expect(renderer!.root.findAllByType('progress')).toHaveLength(0);
  });
}
test('a no-op cancel refreshes the finished transfer instead of claiming cancellation', async () => {
  available = [{ ...model, status: { Downloading: { progress: 99 } } }];
  reply = command => {
    if (command === 'funasr_cancel_download') {
      available = [{ ...model }];
      return false;
    }
    return command === 'funasr_get_available_models' ? available : undefined;
  };
  await show();
  await click('Cancel download');
  expect(errorToast).not.toHaveBeenCalled();
  expect(renderer!.root.findAllByType('progress')).toHaveLength(0);
  expect(renderer!.root.findAllByProps({ title: 'Delete model' }).length).toBeGreaterThan(0);
});
test('an actual failure after requesting Cancel is still reported', async () => {
  available = [{ ...model, status: 'Missing' }];
  let rejectDownload: (error: unknown) => void = () => {};
  const pending = new Promise<void>((_resolve, reject) => { rejectDownload = reject; });
  reply = command => command === 'funasr_download_model' ? pending
    : command === 'funasr_get_available_models' ? available
      : command === 'funasr_cancel_download' ? true : undefined;
  await show();
  const button = renderer!.root.findAllByType('button').find(button => button.props.children?.[1] === 'Download')!;
  let download: Promise<void>;
  await act(async () => { download = button.props.onClick(); });
  await click('Cancel download');
  expect(renderer!.root.findAllByType('progress')).toHaveLength(1);
  await act(async () => { rejectDownload('Checksum mismatch'); await download!; });
  expect(errorToast.mock.calls.length).toBe(1);
});
