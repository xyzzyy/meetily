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
  allowSuccess = false,
  save = async () => ({ meeting_id: 'saved-meeting' }),
  markSaved = async () => {},
  refetch = async () => {},
  getMeeting = async () => ({ title: 'Saved meeting' }),
  deferNavigation = false,
} = {}) {
  const listeners = new Map();
  const statuses = [];
  const subscriptions = new Map();
  const removals = new Map();
  const pendingRegistrations = [];
  const toasts = [];
  const navigationTimers = [];
  const transcriptsRef = { current: [{ text: 'Keep this transcript' }] };
  const activeToasts = new Map();
  const showToast = (...args) => {
    toasts.push(args);
    activeToasts.set(args[1]?.id ?? Symbol('toast'), args);
  };
  const calls = { save: 0, clear: 0, markSaved: 0, flush: 0, poll: 0, transcriptionSuccess: 0, navigation: [], recording: [], disabled: [], active: [] };
  const storage = new Map();
  const recordingStatus = Object.fromEntries([
    'IDLE', 'STOPPING', 'PROCESSING_TRANSCRIPTS', 'SAVING', 'ERROR', 'COMPLETED', 'RECORDING',
  ].map(value => [value, value]));
  const context = {
    status: 'STOPPING',
    setStatus: (status, message) => statuses.push({ status, message }),
  };
  const require = (name) => {
    if (name === 'react') return React;
    if (name === 'next/navigation') return { useRouter: () => ({ push: path => calls.navigation.push(path) }) };
    if (name === '@tauri-apps/api/event') return { listen: (event, callback) => {
      subscriptions.set(event, (subscriptions.get(event) || 0) + 1);
      listeners.set(event, callback);
      const unlisten = () => {
        removals.set(event, (removals.get(event) || 0) + 1);
        if (listeners.get(event) === callback) listeners.delete(event);
      };
      return deferRegistration
        ? new Promise(resolve => pendingRegistrations.push(() => resolve(unlisten)))
        : Promise.resolve(unlisten);
    } };
    if (name === 'sonner') return { toast: {
      error: showToast,
      warning: showToast,
      dismiss: id => activeToasts.delete(id),
      success: (...args) => {
        assert.ok(allowSuccess, 'False save success');
        showToast(...args);
      },
    } };
    if (name === '@/contexts/RecordingStateContext') return {
      useRecordingState: () => context, RecordingStatus: recordingStatus,
    };
    if (name === '@/contexts/TranscriptContext') return { useTranscripts: () => ({
      transcriptsRef,
      flushBuffer: () => calls.flush++,
      clearTranscripts: () => { calls.clear++; transcriptsRef.current = []; },
      markMeetingAsSaved: async () => { calls.markSaved++; return markSaved(); },
    }) };
    if (name === '@/components/Sidebar/SidebarProvider') return { useSidebar: () => ({
      setIsMeetingActive: active => calls.active.push(active),
      refetchMeetings: refetch, setCurrentMeeting() {},
    }) };
    if (name === '@/services/storageService') return { storageService: {
      saveMeeting: async () => { calls.save++; return save(); },
      getMeeting,
    } };
    if (name === '@/lib/summary-language-preferences') return {
      applyPinnedSummaryLanguageToMeeting: async () => true,
    };
    if (name === '@tauri-apps/api/core') return { invoke: async () => ({ enabled: false }) };
    if (name === '@/lib/analytics') return { default: {
      trackTranscriptionSuccess: () => calls.transcriptionSuccess++,
      trackPageView() {}, getMeetingsCountToday: async () => 1,
      trackMeetingCompleted: async () => {}, updateMeetingCount: async () => {},
    } };
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
    setTimeout: (fn, ms) => {
      if (deferNavigation && ms === 2000) navigationTimers.push(fn);
      else fn();
    },
  });
  let hook;
  let renderer;
  function View() {
    // Match callers whose callback props change on every render.
    hook = module.exports.useRecordingStop(
      value => calls.recording.push(value), value => calls.disabled.push(value)
    );
    return null;
  }
  await act(async () => { renderer = create(React.createElement(View)); });
  return {
    get hook() { return hook; },
    listeners, statuses, toasts, activeToasts, calls, storage, transcriptsRef, navigationTimers, context, subscriptions, removals, pendingRegistrations,
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

const transcriptWarning = 'The final transcript may be incomplete. Transcribe saved audio again to recover missing text.';

test('a final transcript warning permits meeting save and replaces the success toast', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: {
    folder_path: '/recordings/Meeting', meeting_name: 'Meeting',
    save_error: null, transcript_warning: transcriptWarning,
  } });
  // Both a UI stop and a tray stop run this same post-processing handler.
  await state.hook.handleRecordingStop(true);
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.markSaved, 1);
  assert.equal(state.calls.transcriptionSuccess, 0);
  assert.equal(state.calls.clear, 1);
  assert.equal(state.toasts.at(-1)[0], 'Meeting saved. Transcript may be incomplete');
  assert.equal(state.toasts.at(-1)[1].description, transcriptWarning);
  assert.equal(state.toasts.at(-1)[1].action.label, 'View Meeting');
  assert.equal(state.statuses.at(-1).status, 'IDLE');
});

test('a late transcript warning replaces success without preventing meeting save', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning } });
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.transcriptionSuccess, 0);
  assert.equal(state.toasts.at(-1)[1].description, transcriptWarning);
});

test('audio save failure remains an error when the final transcript also fails', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: {
    transcript_warning: transcriptWarning, save_error: 'Audio save failed',
  } });
  await state.hook.handleRecordingStop(false);
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.markSaved, 0);
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.toasts.at(-1)[0], 'Recording save failed');
});

test('a new recording clears the transcript warning before a successful stop', async t => {
  const state = await loadHook({ allowSuccess: true });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning } });
  assert.equal(state.activeToasts.get('recording-transcript-incomplete')[1].description, transcriptWarning);
  state.listeners.get('recording-started')({ payload: {} });
  assert.equal(state.activeToasts.has('recording-transcript-incomplete'), false);
  await state.hook.handleRecordingStop(true);
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.transcriptionSuccess, 1);
  assert.equal(state.activeToasts.size, 1);
  assert.equal(state.toasts.at(-1)[0], 'Recording saved successfully!');
});


for (const pendingStep of ['transcription wait', 'meeting save', 'recovery update', 'meeting list', 'meeting fetch']) {
  test(`a new recording during ${pendingStep} cancels the previous stop UI work`, async t => {
    let finishStep;
    const pending = () => new Promise(resolve => { finishStep = resolve; });
    const options = {
      'transcription wait': { poll: pending }, 'meeting save': { save: pending },
      'recovery update': { markSaved: pending }, 'meeting list': { refetch: pending },
      'meeting fetch': { getMeeting: pending },
    };
    const state = await loadHook(options[pendingStep]);
    t.after(state.unmount);
    await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning } });
    const stopping = state.hook.handleRecordingStop(true);
    await new Promise(setImmediate);
    assert.equal(typeof finishStep, 'function');
    state.listeners.get('recording-started')({ payload: {} });
    state.transcriptsRef.current = [{ text: 'New recording transcript' }];
    state.storage.set('indexeddb_current_meeting_id', 'new-recording');
    state.statuses.push({ status: 'RECORDING' });
    const changes = { statuses: state.statuses.length, active: state.calls.active.length, disabled: state.calls.disabled.length };
    finishStep(pendingStep === 'transcription wait'
      ? { is_processing: false, chunks_in_queue: 0 }
      : { meeting_id: 'saved-meeting', title: 'Saved meeting' });
    await stopping;
    assert.equal(state.calls.save, pendingStep === 'transcription wait' ? 0 : 1);
    assert.equal(state.calls.transcriptionSuccess, 0);
    assert.equal(state.calls.clear, 0);
    assert.equal(state.calls.navigation.length, 0);
    assert.equal(state.activeToasts.has('recording-transcript-incomplete'), false);
    assert.equal(state.transcriptsRef.current[0].text, 'New recording transcript');
    assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
    assert.equal(state.statuses.length, changes.statuses);
    assert.equal(state.calls.active.length, changes.active);
    assert.equal(state.calls.disabled.length, changes.disabled);
  });
}

test('a late save error cannot become success when a new recording starts', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Previous save failed' } });
  state.listeners.get('recording-started')({ payload: {} });
  state.statuses.push({ status: 'RECORDING' });
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.transcriptionSuccess, 0);
  assert.equal(state.calls.markSaved, 0);
  assert.equal(state.statuses.at(-1).status, 'RECORDING');
});

test('a new recording cancels the prior stop delayed navigation', async t => {
  const state = await loadHook({ allowSuccess: true, deferNavigation: true });
  t.after(state.unmount);
  await state.hook.handleRecordingStop(true);
  assert.equal(state.navigationTimers.length, 1);
  state.listeners.get('recording-started')({ payload: {} });
  state.transcriptsRef.current = [{ text: 'New recording transcript' }];
  state.storage.set('indexeddb_current_meeting_id', 'new-recording');
  state.statuses.push({ status: 'RECORDING' });
  state.navigationTimers[0]();
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.navigation.length, 0);
  assert.equal(state.transcriptsRef.current[0].text, 'New recording transcript');
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
  assert.equal(state.statuses.at(-1).status, 'RECORDING');
});

test('a successful stop dismisses an earlier transcript warning', async t => {
  const state = await loadHook({ allowSuccess: true });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning } });
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: null } });
  await state.hook.handleRecordingStop(true);
  assert.equal(state.activeToasts.has('recording-transcript-incomplete'), false);
  assert.equal(state.toasts.at(-1)[0], 'Recording saved successfully!');
});

test('an older stop cannot block or release the newer stop guard', async t => {
  const finishSaves = [];
  const state = await loadHook({ allowSuccess: true, save: () => new Promise(resolve => finishSaves.push(resolve)) });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning } });
  const first = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.listeners.get('recording-started')({ payload: {} });
  await state.listeners.get('recording-stopped')({ payload: {} });
  const second = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  assert.equal(finishSaves.length, 2);
  finishSaves[0]({ meeting_id: 'first-meeting' });
  await first;
  await state.hook.handleRecordingStop(true);
  assert.equal(state.calls.save, 2);
  finishSaves[1]({ meeting_id: 'second-meeting' });
  await second;
  assert.equal(state.calls.transcriptionSuccess, 1);
  assert.equal(state.activeToasts.has('recording-transcript-incomplete'), false);
});

test('a rejected old meeting save cannot set the new recording to error', async t => {
  let rejectSave;
  const state = await loadHook({ save: () => new Promise((_, reject) => { rejectSave = reject; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.listeners.get('recording-started')({ payload: {} });
  state.statuses.push({ status: 'RECORDING' });
  rejectSave(new Error('Previous database save failed'));
  await stopping;
  assert.equal(state.statuses.at(-1).status, 'RECORDING');
  assert.equal(state.toasts.length, 0);
  assert.equal(state.calls.clear, 0);
});
