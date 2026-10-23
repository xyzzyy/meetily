import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

// Whisper and Parakeet share the model card and selection rules with FUN-ASR.
const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const dialogs = { ...await import('@tauri-apps/plugin-dialog') };
const notifications = { ...await import('sonner') };
const buttonModule = { ...await import('../../src/components/ui/button') };
const progressModule = { ...await import('../../src/components/ui/progress') };
let reply: (command: string, args?: any) => unknown | Promise<unknown>;
const invoke = mock((command: string, args?: any) => Promise.resolve().then(() => reply(command, args)));
const errorToast = mock(() => {});
const successToast = mock(() => {});
const handlers = new Map<string, (event: any) => void | Promise<void>>();
const confirm = mock(async (_message: string, _options?: unknown) => true);
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => handlers.delete(name);
} }));
mock.module('@tauri-apps/plugin-dialog', () => ({ ...dialogs, confirm }));
mock.module('sonner', () => ({ ...notifications, toast: { error: errorToast, success: successToast, info: mock(() => {}) } }));
mock.module('../../src/components/ui/button', () => ({ ...buttonModule,
  Button: ({ children, ...props }: any) => <button {...props}>{children}</button>,
}));
mock.module('../../src/components/ui/progress', () => ({ ...progressModule,
  Progress: ({ value }: any) => <progress value={value} />,
}));
const { ModelManager } = await import('../../src/components/WhisperModelManager');
const { ParakeetModelManager } = await import('../../src/components/ParakeetModelManager');
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('@tauri-apps/api/event', () => events);
  mock.module('@tauri-apps/plugin-dialog', () => dialogs);
  mock.module('sonner', () => notifications);
  mock.module('../../src/components/ui/button', () => buttonModule);
  mock.module('../../src/components/ui/progress', () => progressModule);
});

const engines = [
  {
    label: 'Whisper', Manager: ModelManager, prefix: 'whisper', provider: 'localWhisper',
    complete: 'model-download-complete', model: { name: 'small', path: '/fixture/small', size_mb: 466,
      accuracy: 'Good', speed: 'Fast', status: 'Available' as any },
  },
  {
    label: 'Parakeet', Manager: ParakeetModelManager, prefix: 'parakeet', provider: 'parakeet',
    complete: 'parakeet-model-download-complete', model: { name: 'parakeet-tdt-0.6b-v3-int8',
      path: '/fixture/parakeet', size_mb: 670, accuracy: 'High', speed: 'Ultra Fast',
      quantization: 'Int8', status: 'Available' as any },
  },
];

let available: any[];
let renderer: ReactTestRenderer | undefined;
const selected = mock((_name: string) => {});
beforeEach(() => {
  invoke.mockClear(); errorToast.mockClear(); successToast.mockClear(); selected.mockClear();
  handlers.clear(); confirm.mockClear();
});
afterEach(async () => {
  if (renderer) await act(async () => renderer!.unmount());
  renderer = undefined;
});

for (const engine of engines) {
  const listCommand = `${engine.prefix}_get_available_models`;
  const respond = (command: string) => command === listCommand ? available : undefined;
  async function show(selectedModel = 'previous') {
    await act(async () => {
      renderer = create(<engine.Manager selectedModel={selectedModel} onModelSelect={selected} autoSave />);
    });
  }
  function selectButton() {
    return renderer!.root.findAllByType('button').find(button => button.props.className?.includes('min-w-0'))!;
  }

  test(`${engine.label}: a failed save keeps the selection and reports the error`, async () => {
    available = [{ ...engine.model }];
    reply = command => {
      if (command === 'api_save_transcript_config') throw new Error('database is locked');
      return respond(command);
    };
    await show();
    await act(async () => { await selectButton().props.onClick(); });
    expect(selected).not.toHaveBeenCalled();
    expect(errorToast.mock.calls.length).toBe(1);
  });

  test(`${engine.label}: selection is reported after the save succeeds`, async () => {
    available = [{ ...engine.model }];
    reply = respond;
    await show();
    expect(selectButton().props['aria-pressed']).toBe(false);
    await act(async () => { await selectButton().props.onClick(); });
    expect(invoke).toHaveBeenCalledWith('api_save_transcript_config', {
      provider: engine.provider, model: engine.model.name, apiKey: null,
    });
    expect(selected).toHaveBeenCalledWith(engine.model.name);
  });

  test(`${engine.label}: reopening during a download shows the engine's progress`, async () => {
    available = [{ ...engine.model, status: { Downloading: { progress: 42 } } }];
    reply = respond;
    await show();
    expect(renderer!.root.findByType('progress').props.value).toBe(42);
    expect(renderer!.root.findAllByProps({ title: 'Cancel download' }).length).toBeGreaterThan(0);
  });

  test(`${engine.label}: a finished download does not change the saved model`, async () => {
    available = [{ ...engine.model, status: 'Missing' }];
    reply = respond;
    await show();
    await act(async () => { await handlers.get(engine.complete)!({ payload: { modelName: engine.model.name } }); });
    expect(selected).not.toHaveBeenCalled();
    expect(invoke.mock.calls.some(([command]) => command === 'api_save_transcript_config')).toBe(false);
    expect(selectButton().props.disabled).toBe(false);
  });

  test(`${engine.label}: damaged files of the selected model can be removed and the note says it is not ready`, async () => {
    available = [{ ...engine.model, status: { Corrupted: { file_size: 1, expected_min_size: 2 } } }];
    reply = command => {
      if (command === `${engine.prefix}_delete_corrupted_model`) available = [{ ...engine.model, status: 'Missing' }];
      return respond(command);
    };
    await show(engine.model.name);
    expect(JSON.stringify(renderer!.toJSON())).toContain('is selected but not ready');
    const remove = renderer!.root.findAllByProps({ title: 'Delete damaged files' }).find(node => node.type === 'button')!;
    await act(async () => { await remove.props.onClick(); });
    expect(confirm).toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith(`${engine.prefix}_delete_corrupted_model`, { modelName: engine.model.name });
    const text = JSON.stringify(renderer!.toJSON());
    expect(text).toContain('is selected but not ready');
    expect(text).not.toContain('for transcription');
  });

  test(`${engine.label}: the selected model must be changed before deletion`, async () => {
    available = [{ ...engine.model }];
    reply = respond;
    await show(engine.model.name);
    expect(selectButton().props['aria-pressed']).toBe(true);
    expect(JSON.stringify(renderer!.toJSON())).toContain('for transcription');
    const remove = renderer!.root.findAllByProps({ title: 'Select another model before deleting this one' })
      .find(node => node.type === 'button')!;
    await act(async () => { await remove.props.onClick(); });
    expect(confirm).not.toHaveBeenCalled();
    expect(errorToast.mock.calls.length).toBe(1);
  });
}

test('Parakeet: a cancel with nothing to cancel refreshes the card instead of waiting for an event', async () => {
  const parakeet = engines[1];
  available = [{ ...parakeet.model, status: { Downloading: { progress: 99 } } }];
  reply = command => {
    if (command === 'parakeet_cancel_download') {
      // The transfer finished before the request: the engine answers without an event.
      available = [{ ...parakeet.model }];
      return 'cancelled';
    }
    return command === 'parakeet_get_available_models' ? available : undefined;
  };
  await act(async () => { renderer = create(<ParakeetModelManager selectedModel="previous" onModelSelect={selected} autoSave />); });
  const cancel = renderer!.root.findAllByProps({ title: 'Cancel download' }).find(node => node.type === 'button')!;
  await act(async () => { await cancel.props.onClick(); });
  const text = JSON.stringify(renderer!.toJSON());
  expect(text).not.toContain('Cancellation requested');
  expect(text).toContain('Ready');
  expect(renderer!.root.findAllByType('progress')).toHaveLength(0);
});
