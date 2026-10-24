import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

/** Mount the real RecordingControls with a controllable Tauri `listen`. */
function load() {
  const live = new Set();
  const pending = [];
  const registered = [];
  const state = { manual: false };
  const listen = (event, handler) => {
    const token = { event, handler };
    registered.push(token);
    return new Promise(resolve => {
      const finish = () => { live.add(token); resolve(() => live.delete(token)); };
      if (state.manual) pending.push(finish); else finish();
    });
  };
  const anything = () => new Proxy(function () {}, { get: () => anything(), apply: () => undefined });
  const passthrough = ({ children }) => children ?? null;
  const source = fs.readFileSync(new URL('../../src/components/RecordingControls.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(compiled, {
    module, exports: module.exports, React, console: { log() {}, error() {}, warn() {} },
    setTimeout, clearTimeout, setInterval, clearInterval,
    require: name => {
      if (name === 'react') return React;
      if (name === '@tauri-apps/api/event') return { listen };
      if (name === '@tauri-apps/api/core') return { invoke: async () => undefined };
      if (name === '@tauri-apps/api/path') return { appDataDir: async () => '/tmp' };
      if (name === 'lucide-react') return new Proxy({}, { get: () => () => null });
      if (name === '@/components/ui/alert') return { Alert: passthrough, AlertDescription: passthrough, AlertTitle: passthrough };
      if (name === '@/components/ui/tooltip') {
        return { Tooltip: passthrough, TooltipContent: () => null, TooltipProvider: passthrough, TooltipTrigger: passthrough };
      }
      if (name === '@/lib/analytics') return { default: anything(), __esModule: true };
      if (name === '@/contexts/RecordingStateContext') {
        return { useRecordingState: () => ({ isPaused: false, isStartingRecording: false, isRecording: false }) };
      }
      return {};
    },
  });
  const flush = () => act(async () => { await new Promise(resolve => setImmediate(resolve)); });
  const props = overrides => ({
    isRecording: false, barHeights: [], onRecordingStart() {}, onTranscriptReceived() {},
    isRecordingDisabled: false, isParentProcessing: false, onRecordingStop() {}, ...overrides,
  });
  return { live, pending, registered, state, flush, props, Controls: module.exports.RecordingControls };
}

async function drain(app) {
  while (app.pending.length) {
    app.pending.shift()();
    await app.flush();
  }
}

test('re-rendering with new callback functions does not add listeners', async () => {
  const app = load();
  let renderer;
  await act(async () => { renderer = create(React.createElement(app.Controls, app.props({ onRecordingStop() {} }))); });
  await app.flush();
  for (let render = 0; render < 5; render++) {
    await act(async () => { renderer.update(React.createElement(app.Controls, app.props({ onRecordingStop() {} }))); });
    await app.flush();
  }
  assert.equal(app.registered.length, 3, 'one registration per event, however often the parent renders');
  assert.equal(app.live.size, 3);
  await act(async () => renderer.unmount());
  assert.equal(app.live.size, 0, 'unmounting removes every listener');
});

test('listeners that finish registering after unmount remove themselves', async () => {
  const app = load();
  app.state.manual = true;
  let renderer;
  await act(async () => { renderer = create(React.createElement(app.Controls, app.props())); });
  await act(async () => renderer.unmount());
  await drain(app);
  await app.flush();
  assert.equal(app.live.size, 0, 'no listener is left running after the component is gone');
});

test('a transcript error calls the latest stop handler', async () => {
  const app = load();
  const stops = [];
  let renderer;
  await act(async () => { renderer = create(React.createElement(app.Controls, app.props({ onRecordingStop: () => stops.push('first') }))); });
  await app.flush();
  await act(async () => { renderer.update(React.createElement(app.Controls, app.props({ onRecordingStop: () => stops.push('second') }))); });
  const handler = app.registered.find(token => token.event === 'transcript-error').handler;
  await act(async () => handler({ payload: 'boom' }));
  assert.deepEqual(stops, ['second']);
  await act(async () => renderer.unmount());
});
