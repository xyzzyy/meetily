import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

async function mountState() {
  const listeners = new Map();
  const polls = [];
  const intervals = new Map();
  let timerId = 0;
  const recordingService = new Proxy({}, { get: (_, method) => {
    if (method === 'getRecordingState') return () => new Promise(resolve => polls.push(resolve));
    return async callback => {
      listeners.set(method, callback);
      return () => listeners.delete(method);
    };
  } });
  const source = fs.readFileSync(new URL('../../src/contexts/RecordingStateContext.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(compiled, {
    module, exports: module.exports,
    require: name => {
      if (name === 'react') return React;
      if (name === '@/services/recordingService') return { recordingService };
      if (name === 'sonner') return { toast: {} };
      throw new Error(`Unexpected import: ${name}`);
    },
    console: { log() {}, error() {} },
    setInterval: callback => { const id = ++timerId; intervals.set(id, callback); return id; },
    clearInterval: id => intervals.delete(id),
  });
  let state;
  let renderer;
  function View() { state = module.exports.useRecordingState(); return null; }
  await act(async () => {
    renderer = create(React.createElement(module.exports.RecordingStateProvider, null, React.createElement(View)));
  });
  return {
    listeners, polls, intervals,
    get state() { return state; },
    unmount: () => act(async () => renderer.unmount()),
  };
}

const active = {
  is_recording: true, is_active: true, is_paused: false,
  recording_duration: 10, active_duration: 10,
};

test('a poll that finishes after fatal shutdown cannot restore recording controls', async t => {
  const app = await mountState();
  t.after(app.unmount);
  await act(async () => app.listeners.get('onRecordingStarted')());
  // Begin a periodic poll while recording is active.
  app.intervals.values().next().value();
  await act(async () => app.listeners.get('onRecordingStopped')({ save_error: null }));
  await act(async () => app.polls.forEach(resolve => resolve(active)));
  assert.equal(app.state.isRecording, false);
  assert.equal(app.state.isActive, false);
  assert.equal(app.intervals.size, 0);
});

test('a poll from the prior recording cannot clear a new recording', async t => {
  const app = await mountState();
  t.after(app.unmount);
  await act(async () => app.listeners.get('onRecordingStarted')());
  await act(async () => app.polls[0]({ ...active, is_recording: false, is_active: false }));
  assert.equal(app.state.isRecording, true);
});

test('a current poll still updates recording state', async t => {
  const app = await mountState();
  t.after(app.unmount);
  await act(async () => app.polls[0](active));
  assert.equal(app.state.isRecording, true);
  assert.equal(app.state.recordingDuration, 10);
});
