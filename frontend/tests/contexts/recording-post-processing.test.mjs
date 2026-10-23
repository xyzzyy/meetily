import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

/** Mount the provider with controllable event registration and stop callbacks. */
async function mountProvider({ deferRegistration = false } = {}) {
  const listeners = new Map();
  const pending = [];
  let subscriptions = 0;
  let removals = 0;
  let handler = () => {};
  let renderer;
  const source = fs.readFileSync(new URL('../../src/contexts/RecordingPostProcessingProvider.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  });
  const require = name => {
    if (name === 'react') return React;
    if (name === '@/hooks/useRecordingStop') return { useRecordingStop: () => {
      const current = handler;
      return { handleRecordingStop: value => current(value) };
    } };
    if (name === '@tauri-apps/api/event') return { listen: (event, callback) => {
      assert.equal(event, 'recording-stop-complete');
      const id = ++subscriptions;
      listeners.set(id, callback);
      const unlisten = () => { removals++; listeners.delete(id); };
      return deferRegistration
        ? new Promise(resolve => pending.push(() => resolve(unlisten)))
        : Promise.resolve(unlisten);
    } };
    throw new Error(`Unexpected import: ${name}`);
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, {
    module, exports: module.exports, require, console: { log() {}, error() {} },
  });
  const Provider = module.exports.RecordingPostProcessingProvider;
  const view = () => React.createElement(Provider, null, 'Meeting');
  await act(async () => { renderer = create(view()); });
  return {
    listeners, pending,
    get subscriptions() { return subscriptions; },
    get removals() { return removals; },
    render: next => act(async () => { handler = next; renderer.update(view()); }),
    unmount: () => act(async () => { renderer.unmount(); }),
  };
}

test('tray stop listener stays mounted across renders and calls the latest handler', async () => {
  const state = await mountProvider();
  const calls = [];
  try {
    for (let render = 0; render < 20; render++) {
      await state.render(value => calls.push([render, value]));
      assert.equal(state.subscriptions, 1);
      assert.equal(state.removals, 0);
      assert.equal(state.listeners.size, 1);
    }
    await state.listeners.get(1)({ payload: true });
    assert.deepEqual(calls, [[19, true]]);
  } finally {
    await state.unmount();
  }
  assert.equal(state.listeners.size, 0);
  assert.equal(state.removals, 1);
});

test('registration completed after unmount releases the tray stop listener', async () => {
  const state = await mountProvider({ deferRegistration: true });
  const calls = [];
  await state.render(value => calls.push(value));
  const lateEvent = state.listeners.get(1);
  await state.unmount();
  lateEvent({ payload: true });
  assert.deepEqual(calls, []);
  await act(async () => { state.pending.forEach(complete => complete()); });
  assert.equal(state.listeners.size, 0);
  assert.equal(state.removals, 1);
});
