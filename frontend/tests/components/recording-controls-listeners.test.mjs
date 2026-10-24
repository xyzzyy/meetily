import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

function compile(file) {
  const source = fs.readFileSync(new URL(`../../src/${file}`, import.meta.url), 'utf8');
  return ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
}

/** Mount the real RecordingControls and its real useTauriEvent hook with a fake Tauri `listen`. */
function load() {
  const live = new Map();
  const registered = [];
  const calls = { stopped: [], errors: [] };
  const listen = async (event, handler) => {
    registered.push(event);
    live.set(event, handler);
    return () => live.delete(event);
  };
  const hook = { exports: {} };
  vm.runInNewContext(compile('hooks/useTauriEvent.ts'), {
    module: hook, exports: hook.exports, console,
    require: name => (name === 'react' ? React : { listen }),
  });
  const anything = () => new Proxy(function () {}, { get: () => anything(), apply: () => undefined });
  const passthrough = ({ children }) => children ?? null;
  const module = { exports: {} };
  vm.runInNewContext(compile('components/RecordingControls.tsx'), {
    module, exports: module.exports, React, console: { log() {}, error() {}, warn() {} },
    setTimeout, clearTimeout, setInterval, clearInterval,
    require: name => {
      if (name === 'react') return React;
      if (name === '@/hooks/useTauriEvent') return hook.exports;
      if (name === '@tauri-apps/api/core') return { invoke: async () => undefined };
      if (name === '@tauri-apps/api/path') return { appDataDir: async () => '/tmp' };
      if (name === 'lucide-react') return new Proxy({}, { get: () => () => null });
      if (name === '@/components/ui/alert') return { Alert: passthrough, AlertDescription: passthrough, AlertTitle: passthrough };
      if (name === '@/components/ui/tooltip') {
        return { Tooltip: passthrough, TooltipContent: () => null, TooltipProvider: passthrough, TooltipTrigger: passthrough };
      }
      if (name === '@/lib/analytics') {
        return { __esModule: true, default: new Proxy({}, {
          get: (_, method) => (method === 'trackTranscriptionError' ? message => calls.errors.push(message) : anything()),
        }) };
      }
      if (name === '@/contexts/RecordingStateContext') {
        return { useRecordingState: () => ({ isPaused: false, isStartingRecording: false, isRecording: false }) };
      }
      return {};
    },
  });
  const props = () => ({
    isRecording: true, barHeights: [], onRecordingStart() {}, onTranscriptReceived() {},
    isRecordingDisabled: false, isParentProcessing: false, onRecordingStop: value => calls.stopped.push(value),
  });
  const flush = () => act(async () => { await new Promise(resolve => setImmediate(resolve)); });
  return { live, registered, calls, props, flush, Controls: module.exports.RecordingControls };
}

async function mount() {
  const app = load();
  let renderer;
  await act(async () => { renderer = create(React.createElement(app.Controls, app.props())); });
  await app.flush();
  return { ...app, rerender: () => act(async () => renderer.update(React.createElement(app.Controls, app.props()))), unmount: () => act(async () => renderer.unmount()) };
}

test('listens once for transcription errors and speech, however often the parent renders', async () => {
  const app = await mount();
  for (let render = 0; render < 5; render++) await app.rerender();
  await app.flush();
  assert.deepEqual([...app.registered].sort(), ['speech-detected', 'transcription-error']);
  await app.unmount();
  assert.equal(app.live.size, 0, 'unmounting removes both listeners');
});

test('a fatal worker error waits for native stop completion before post-processing', async () => {
  const app = await mount();
  await act(async () => app.live.get('transcription-error')({ payload: {
    phase: 'active', actionable: true, error: 'Engine initialization failed',
    userMessage: 'Speech recognition failed. Recording is stopping.',
  } }));
  assert.deepEqual(app.calls.stopped, []);
  assert.deepEqual(app.calls.errors, ['Speech recognition failed. Recording is stopping.']);
  await app.unmount();
});

test('a nonfatal chunk error does not end the recording in the interface', async () => {
  const app = await mount();
  await act(async () => app.live.get('transcription-error')({ payload: {
    phase: 'active', actionable: false, error: 'One chunk failed', userMessage: 'One chunk failed',
  } }));
  assert.deepEqual(app.calls.stopped, []);
  assert.deepEqual(app.calls.errors, ['One chunk failed']);
  await app.unmount();
});
