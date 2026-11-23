import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

async function mountProvider({ pendingOpen = false, deferRegistration = false } = {}) {
  const listeners = new Map();
  const requests = [];
  const registrations = [];
  const meetings = new Map();
  const transcripts = [];
  const storage = new Map();
  const complete = result => {
    const request = { result };
    queueMicrotask(() => request.onsuccess?.());
    return request;
  };
  const database = { transaction: () => ({ objectStore: name => name === 'meetings' ? {
    put: value => { meetings.set(value.meetingId, { ...value }); return complete(undefined); },
    get: key => complete(meetings.get(key)),
  } : {
    add: value => { transcripts.push(value); return complete(transcripts.length); },
  } }) };
  const quietConsole = { log() {}, warn() {}, error() {} };
  const compile = path => ts.transpileModule(fs.readFileSync(new URL(path, import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const serviceModule = { exports: {} };
  vm.runInNewContext(compile('../../src/services/indexedDBService.ts'), {
    module: serviceModule, exports: serviceModule.exports, console: quietConsole,
    indexedDB: { open: () => {
      const request = { result: database, error: new Error('Temporary storage failure') };
      requests.push(request);
      if (!pendingOpen) queueMicrotask(() => {
        if (requests.length === 1) request.onerror();
        else request.onsuccess();
      });
      return request;
    } },
  });
  const subscribe = (name, callback) => {
    listeners.set(name, callback);
    const unlisten = () => { if (listeners.get(name) === callback) listeners.delete(name); };
    return deferRegistration && name === 'started'
      ? new Promise(resolve => registrations.push(() => resolve(unlisten)))
      : Promise.resolve(unlisten);
  };
  const require = name => {
    if (name === 'react') return React;
    if (name === 'sonner') return { toast: { success() {} } };
    if (name === './RecordingStateContext') return { useRecordingState: () => ({ isRecording: false }) };
    if (name === '@/services/indexedDBService') return serviceModule.exports;
    if (name === '@/services/recordingService') return { recordingService: {
      onRecordingStarted: callback => subscribe('started', callback),
      onRecordingStopped: callback => subscribe('stopped', callback),
      getRecordingMeetingName: async () => 'Recovery test',
    } };
    if (name === '@/services/transcriptService') return { transcriptService: {
      onTranscriptUpdate: callback => subscribe('transcript', callback),
    } };
    if (name === '@tauri-apps/api/core') return { invoke: async () => '/recordings/Recovery' };
    throw new Error(`Unexpected import: ${name}`);
  };
  const module = { exports: {} };
  vm.runInNewContext(compile('../../src/contexts/TranscriptContext.tsx'), {
    module, exports: module.exports, require, console: quietConsole,
    setTimeout, clearTimeout, alert: assert.fail,
    sessionStorage: {
      setItem: (key, value) => storage.set(key, value),
      getItem: key => storage.get(key), removeItem: key => storage.delete(key),
    },
  });
  let renderer;
  await act(async () => { renderer = create(React.createElement(module.exports.TranscriptProvider)); });
  return {
    listeners, requests, registrations, meetings, transcripts, storage,
    unmount: () => act(async () => renderer.unmount()),
  };
}

test('recovery listeners survive an initial storage failure and the next recording retries storage', async t => {
  const state = await mountProvider();
  t.after(state.unmount);
  assert.equal(state.requests.length, 1);
  assert.ok(state.listeners.has('started'), 'storage failure must not disable recording recovery');
  assert.ok(state.listeners.has('stopped'));
  await act(async () => { await state.listeners.get('started')(); });
  const meetingId = state.storage.get('indexeddb_current_meeting_id');
  assert.equal(state.requests.length, 2);
  assert.ok(meetingId);
  assert.equal(state.meetings.get(meetingId).title, 'Recovery test');
  assert.equal(state.meetings.get(meetingId).folderPath, '/recordings/Recovery');
  await act(async () => {
    state.listeners.get('transcript')({ sequence_id: 1, text: 'Keep this speech', timestamp: '00:00' });
    await new Promise(resolve => setTimeout(resolve, 20));
  });
  assert.equal(state.transcripts.length, 1);
  assert.equal(state.transcripts[0].meetingId, meetingId);
  assert.equal(state.meetings.get(meetingId).transcriptCount, 1);
});

test('a pending storage open does not delay registration of recording listeners', async t => {
  const state = await mountProvider({ pendingOpen: true });
  t.after(state.unmount);
  assert.ok(state.listeners.has('started'));
  assert.ok(state.listeners.has('stopped'));
  await act(async () => state.requests[0].onerror());
});

test('recording listener registration completed after unmount is released', async () => {
  const state = await mountProvider({ deferRegistration: true });
  assert.equal(state.registrations.length, 1);
  await state.unmount();
  await act(async () => state.registrations.forEach(finish => finish()));
  assert.equal(state.listeners.size, 0);
});
