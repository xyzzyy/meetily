import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

/** Mount the real hook with test event, storage, and status services. */
async function loadHook({
  poll = async () => ({ is_processing: false, chunks_in_queue: 0 }),
  deferRegistration = false,
} = {}) {
  const listeners = new Map();
  const statuses = [];
  const subscriptions = new Map();
  const removals = new Map();
  const pendingRegistrations = [];
  const toasts = [];
  const calls = { save: 0, clear: 0, markSaved: 0, flush: 0, poll: 0 };
  const storage = new Map();
  const recordingStatus = Object.fromEntries([
    'IDLE', 'STOPPING', 'PROCESSING_TRANSCRIPTS', 'SAVING', 'ERROR', 'COMPLETED',
  ].map(value => [value, value]));
  const context = {
    status: 'STOPPING',
    setStatus: (status, message) => statuses.push({ status, message }),
  };
  const require = (name) => {
    if (name === 'react') return React;
    if (name === 'next/navigation') return { useRouter: () => ({ push() {} }) };
    if (name === '@tauri-apps/api/event') return { listen: (event, callback) => {
      subscriptions.set(event, (subscriptions.get(event) || 0) + 1);
      listeners.set(event, callback);
      const unlisten = () => {
        removals.set(event, (removals.get(event) || 0) + 1);
        listeners.delete(event);
      };
      return deferRegistration
        ? new Promise(resolve => pendingRegistrations.push(() => resolve(unlisten)))
        : Promise.resolve(unlisten);
    } };
    if (name === 'sonner') return { toast: {
      error: (...args) => toasts.push(args), success: () => assert.fail('False save success'),
    } };
    if (name === '@/contexts/RecordingStateContext') return {
      useRecordingState: () => context, RecordingStatus: recordingStatus,
    };
    if (name === '@/contexts/TranscriptContext') return { useTranscripts: () => ({
      transcriptsRef: { current: [{ text: 'Keep this transcript' }] },
      flushBuffer: () => calls.flush++, clearTranscripts: () => calls.clear++,
      markMeetingAsSaved: async () => calls.markSaved++,
    }) };
    if (name === '@/components/Sidebar/SidebarProvider') return { useSidebar: () => ({ setIsMeetingActive() {} }) };
    if (name === '@/services/storageService') return { storageService: { saveMeeting: async () => calls.save++ } };
    if (name === '@/services/transcriptService') return { transcriptService: {
      getTranscriptionStatus: async () => {
        calls.poll++;
        return poll();
      },
    } };
    return {};
  };
  const source = fs.readFileSync(new URL('../../src/hooks/useRecordingStop.ts', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, {
    module, exports: module.exports, require, window: {}, console: { log() {}, warn() {}, error() {} },
    sessionStorage: {
      setItem: (key, value) => storage.set(key, value),
      getItem: key => storage.get(key), removeItem: key => storage.delete(key),
    },
    setTimeout: fn => fn(),
  });
  let hook;
  let renderer;
  function View() {
    // Match callers whose callback props change on every render.
    hook = module.exports.useRecordingStop(() => {}, () => {});
    return null;
  }
  await act(async () => { renderer = create(React.createElement(View)); });
  return {
    get hook() { return hook; },
    listeners, statuses, toasts, calls, storage, context, subscriptions, removals, pendingRegistrations,
    render: () => act(async () => { renderer.update(React.createElement(View)); }),
    unmount: () => act(async () => { renderer.unmount(); }),
  };
}

test('a save error shows the recovery folder and keeps the transcript recovery copy', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  const message = 'Recording save failed. Files kept in: /recordings/Meeting. Audio may be incomplete.';
  await state.listeners.get('recording-stopped')({ payload: {
    folder_path: '/recordings/Meeting', meeting_name: 'Meeting', save_error: message,
  } });
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.toasts[0][1].description, message);
  assert.equal(state.storage.get('last_recording_folder_path'), '/recordings/Meeting');

  // Main window calls this after the stop command rejects. Tray stops use the event alone.
  await state.hook.handleRecordingStop(false);
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.statuses.at(-1).message, message);
  assert.equal(state.calls.poll, 0);
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.markSaved, 0);
  assert.ok(state.calls.flush > 0);
});

test('a later successful stop clears the prior save error', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Write failed' } });
  await state.listeners.get('recording-stopped')({ payload: { save_error: null } });
  await state.hook.handleRecordingStop(false);
  assert.equal(state.calls.poll, 1);
  assert.equal(state.statuses.at(-1).status, 'IDLE');
});

test('a save error that arrives during post-processing prevents a success result', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  assert.equal(typeof finishPoll, 'function');
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Final audio write failed' } });
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.markSaved, 0);
});

test('subscriptions stay active across renders and use the latest callbacks', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  const latestStatuses = [];
  for (let i = 0; i < 20; i++) {
    state.context.setStatus = (status, message) => latestStatuses.push({ status, message });
    await state.render();
    assert.equal(state.subscriptions.get('recording-stopped'), 1);
    assert.equal(state.subscriptions.get('recording-started'), 1);
    assert.equal(state.removals.size, 0);
  }
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Current write failed' } });
  assert.equal(latestStatuses.at(-1).status, 'ERROR');
  assert.equal(state.statuses.length, 0);
});

test('a new recording clears the prior error before its stop event arrives', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Previous write failed' } });
  state.listeners.get('recording-started')({ payload: {} });
  // A transcription error can call this before the next recording-stopped event.
  await state.hook.handleRecordingStop(false);
  assert.equal(state.calls.poll, 1);
  assert.equal(state.statuses.at(-1).status, 'IDLE');
});

test('duplicate post-processing cannot erase a failed save in the same recording', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Current write failed' } });
  await state.hook.handleRecordingStop(false);
  await state.hook.handleRecordingStop(true);
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.markSaved, 0);
});

test('late listener registration is removed after unmount', async () => {
  const state = await loadHook({ deferRegistration: true });
  await state.unmount();
  await act(async () => { state.pendingRegistrations.forEach(complete => complete()); });
  assert.equal(state.listeners.size, 0);
  assert.equal(state.removals.get('recording-stopped'), 1);
  assert.equal(state.removals.get('recording-started'), 1);
});
