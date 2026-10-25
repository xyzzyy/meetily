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
  deferDelayMs,
  loadStored = async () => [],
} = {}) {
  const listeners = new Map();
  const statuses = [];
  const subscriptions = new Map();
  const removals = new Map();
  const pendingRegistrations = [];
  const toasts = [];
  const navigationTimers = [];
  const transcriptsRef = { current: [{ id: 'old-segment', sequence_id: 1, text: 'Keep this transcript', timestamp: '00:00' }] };
  const delayedWaits = [];
  const activeToasts = new Map();
  const showToast = (...args) => {
    toasts.push(args);
    activeToasts.set(args[1]?.id ?? Symbol('toast'), args);
  };
  const calls = { saveAcks: [], save: 0, clear: 0, markSaved: 0, flush: 0, poll: 0, transcriptionSuccess: 0, meetingsCompleted: 0, markedIds: [], savedMeetings: [], recoveryReads: [], refreshes: 0, navigation: [], recording: [], disabled: [], active: [] };
  const storage = new Map([['indexeddb_current_meeting_id', 'old-recording']]);
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
      transcriptsRef, currentMeetingId: null,
      flushBuffer: () => calls.flush++,
      clearTranscripts: () => { calls.clear++; transcriptsRef.current = []; },
      markMeetingAsSaved: async id => {
        calls.markSaved++;
        calls.markedIds.push(id);
        return markSaved();
      },
    }) };
    if (name === '@/components/Sidebar/SidebarProvider') return { useSidebar: () => ({
      setIsMeetingActive: active => calls.active.push(active),
      refetchMeetings: async () => { calls.refreshes++; return refetch(); }, setCurrentMeeting() {},
    }) };
    if (name === '@/services/storageService') return { storageService: {
      saveMeeting: async (...args) => { calls.save++; calls.savedMeetings.push(args); return save(...args); },
      getMeeting,
    } };
    if (name === '@/services/indexedDBService') return { indexedDBService: {
      getTranscripts: async id => { calls.recoveryReads.push(id); return loadStored(id); },
    } };
    if (name === '@/lib/summary-language-preferences') return {
      applyPinnedSummaryLanguageToMeeting: async () => true,
    };
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
      if (command === 'complete_recording_save') calls.saveAcks.push(args.saveToken);
      return { enabled: false };
    } };
    if (name === '@/lib/analytics') return { default: {
      trackTranscriptionSuccess: () => calls.transcriptionSuccess++,
      trackPageView() {}, getMeetingsCountToday: async () => 1,
      trackMeetingCompleted: async () => { calls.meetingsCompleted++; }, updateMeetingCount: async () => {},
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
      else if (deferDelayMs === ms) delayedWaits.push(fn);
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
    listeners, statuses, toasts, activeToasts, calls, storage, transcriptsRef, navigationTimers, delayedWaits, context, subscriptions, removals, pendingRegistrations,
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


for (const pendingStep of ['transcription wait', 'recovery read', 'meeting save', 'recovery update', 'meeting list', 'meeting fetch']) {
  test(`a new recording during ${pendingStep} cancels the previous stop UI work`, async t => {
    let finishStep;
    const pending = () => new Promise(resolve => { finishStep = resolve; });
    const options = {
      'transcription wait': { poll: pending }, 'recovery read': { loadStored: pending }, 'meeting save': { save: pending },
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
      : pendingStep === 'recovery read' ? []
      : { meeting_id: 'saved-meeting', title: 'Saved meeting' });
    await stopping;
    assert.equal(state.calls.save, 1);
    assert.equal(state.calls.markSaved, 1);
    assert.deepEqual(state.calls.markedIds, ['old-recording']);
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
  await state.listeners.get('recording-stopped')({ payload: { transcript_warning: transcriptWarning, save_token: 'old-token' } });
  const first = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.listeners.get('recording-started')({ payload: {} });
  await state.listeners.get('recording-stopped')({ payload: { save_token: 'new-token' } });
  const second = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  assert.equal(finishSaves.length, 2);
  assert.deepEqual(state.calls.saveAcks, []);
  finishSaves[0]({ meeting_id: 'first-meeting' });
  await first;
  assert.deepEqual(state.calls.saveAcks, ['old-token']);
  await state.hook.handleRecordingStop(true);
  assert.equal(state.calls.save, 2);
  finishSaves[1]({ meeting_id: 'second-meeting' });
  await second;
  assert.deepEqual(state.calls.saveAcks, ['old-token', 'new-token']);
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
  // The user still learns that the earlier meeting was not saved.
  assert.equal(state.toasts.length, 1);
  assert.equal(state.toasts[0][0], 'Failed to save the previous meeting');
  assert.match(state.toasts[0][1].description, /Previous database save failed/);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.markSaved, 0);
});

for (const pendingStep of ['transcription wait', 'meeting save']) {
  test(`page unmount during ${pendingStep} keeps the recording save active`, async t => {
    let finishStep;
    const pending = () => new Promise(resolve => { finishStep = resolve; });
    const state = await loadHook(pendingStep === 'meeting save' ? { save: pending } : { poll: pending });
    t.after(state.unmount);
    const stopping = state.hook.handleRecordingStop(true);
    await new Promise(setImmediate);
    await state.unmount();
    assert.equal(typeof state.listeners.get('recording-started'), 'function');
    finishStep(pendingStep === 'meeting save'
      ? { meeting_id: 'saved-meeting' }
      : { is_processing: false, chunks_in_queue: 0 });
    await stopping;
    assert.equal(state.calls.save, 1);
    assert.equal(state.calls.markSaved, 1);
    assert.deepEqual(state.calls.markedIds, ['old-recording']);
    // The saved meeting's transcript is cleared, but the user is not navigated away.
    assert.equal(state.calls.clear, 1);
    assert.equal(state.calls.navigation.length, 0);
    assert.equal(state.toasts.length, 0);
    assert.equal(state.statuses.at(-1).status, 'IDLE');
    assert.equal(state.calls.active.at(-1), false);
    assert.equal(state.calls.meetingsCompleted, 1);
    assert.equal(state.calls.transcriptionSuccess, 1);
    assert.equal(state.listeners.size, 0);
  });
}

test('page unmount before a requested stop returns keeps its save error and start events', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  // The Stop button marks the stop before stop_recording returns.
  state.hook.setIsStopping(true);
  await state.unmount();
  assert.equal(typeof state.listeners.get('recording-stopped'), 'function');
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Audio save failed' } });
  await state.hook.handleRecordingStop(false);
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.statuses.at(-1).message, 'Audio save failed');
  assert.equal(state.calls.poll, 0);
  assert.equal(state.listeners.size, 0);
});

test('a new recording after an unmounted requested stop cancels the old stop UI work', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  state.hook.setIsStopping(true);
  await state.unmount();
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.listeners.get('recording-started')({ payload: {} });
  state.statuses.push({ status: 'RECORDING' });
  const changes = { statuses: state.statuses.length, active: state.calls.active.length };
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.statuses.length, changes.statuses);
  assert.equal(state.calls.active.length, changes.active);
  assert.equal(state.listeners.size, 0);
});

test('a requested stop that never reaches the handler releases listeners at the next start', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  state.hook.setIsStopping(true);
  await state.unmount();
  assert.equal(state.listeners.size, 2);
  state.listeners.get('recording-started')({ payload: {} });
  assert.equal(state.listeners.size, 0);
});

test('a finished stop request without post-processing releases listeners after unmount', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  state.hook.setIsStopping(true);
  await state.unmount();
  assert.equal(state.listeners.size, 2);
  state.statuses.push({ status: 'SAVING' });
  state.hook.setIsStopping(false);
  assert.equal(state.listeners.size, 0);
  assert.equal(state.statuses.at(-1).status, 'SAVING');
});

test('a finished request keeps an unmounted handler listening until its save ends', async t => {
  let finishSave;
  const state = await loadHook({ save: () => new Promise(resolve => { finishSave = resolve; }) });
  t.after(state.unmount);
  state.hook.setIsStopping(true);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.unmount();
  state.hook.setIsStopping(false);
  assert.equal(typeof state.listeners.get('recording-stopped'), 'function');
  finishSave({ meeting_id: 'saved-meeting' });
  await stopping;
  assert.equal(state.calls.save, 1);
  assert.equal(state.listeners.size, 0);
});

test('an unsaved stop after unmount still ends the meeting', async t => {
  const state = await loadHook();
  t.after(state.unmount);
  state.hook.setIsStopping(true);
  await state.unmount();
  await state.hook.handleRecordingStop(false);
  assert.equal(state.statuses.at(-1).status, 'IDLE');
  assert.equal(state.calls.active.at(-1), false);
  assert.equal(state.calls.save, 0);
});

test('unmount before delayed navigation still resets the saved recording', async t => {
  const state = await loadHook({ allowSuccess: true, deferNavigation: true });
  t.after(state.unmount);
  await state.hook.handleRecordingStop(true);
  assert.equal(state.navigationTimers.length, 1);
  await state.unmount();
  state.navigationTimers[0]();
  assert.equal(state.calls.navigation.length, 0);
  assert.equal(state.calls.clear, 1);
  assert.equal(state.statuses.at(-1).status, 'IDLE');
  assert.equal(state.listeners.size, 0);
});

test('a new recording after unmount is protected from the old navigation timer', async t => {
  const state = await loadHook({ allowSuccess: true, deferNavigation: true });
  t.after(state.unmount);
  await state.hook.handleRecordingStop(true);
  await state.unmount();
  // The sidebar can open Home and start a new recording without reloading the app.
  assert.equal(typeof state.listeners.get('recording-started'), 'function');
  state.listeners.get('recording-started')({ payload: {} });
  state.storage.set('indexeddb_current_meeting_id', 'new-recording');
  state.transcriptsRef.current = [{ id: 'new-segment', text: 'New recording transcript' }];
  state.statuses.push({ status: 'RECORDING' });
  state.navigationTimers[0]();
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.navigation.length, 0);
  assert.equal(state.transcriptsRef.current[0].text, 'New recording transcript');
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
  assert.equal(state.statuses.at(-1).status, 'RECORDING');
  assert.equal(state.listeners.size, 0);
});

test('a cleared context before the final snapshot keeps the stop transcript', async t => {
  const state = await loadHook({ deferDelayMs: 500 });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  assert.equal(state.delayedWaits.length, 1);
  // A new start can clear the context before its recording-started event arrives.
  state.transcriptsRef.current = [];
  state.delayedWaits[0]();
  await stopping;
  assert.equal(state.calls.savedMeetings[0][1].map(t => t.text).join('|'), 'Keep this transcript');
});

test('a new recording after page unmount preserves the old save and the new recovery ID', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.unmount();
  state.listeners.get('recording-started')({ payload: {} });
  state.storage.set('indexeddb_current_meeting_id', 'new-recording');
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.markSaved, 1);
  assert.deepEqual(state.calls.markedIds, ['old-recording']);
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
  assert.equal(state.listeners.size, 0);
});

test('a database save failure after unmount keeps recovery and reports an error', async t => {
  let rejectSave;
  const state = await loadHook({ save: () => new Promise((_, reject) => { rejectSave = reject; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.unmount();
  rejectSave(new Error('Database write failed'));
  await stopping;
  assert.equal(state.calls.markSaved, 0);
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'old-recording');
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.calls.clear, 0);
  assert.equal(state.listeners.size, 0);
});

test('a late audio save error after unmount still prevents recovery cleanup', async t => {
  let finishPoll;
  const state = await loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  await state.unmount();
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Final audio write failed' } });
  finishPoll({ is_processing: false, chunks_in_queue: 0 });
  await stopping;
  assert.equal(state.calls.save, 0);
  assert.equal(state.calls.markSaved, 0);
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'old-recording');
  assert.equal(state.statuses.at(-1).status, 'ERROR');
  assert.equal(state.listeners.size, 0);
});

test('a new recording during the wait saves the old transcript, final segments, and folder', async t => {
  let finishPoll;
  const state = await loadHook({
    poll: () => new Promise(resolve => { finishPoll = resolve; }),
    loadStored: async () => [
      { id: 1, meetingId: 'old-recording', sequenceId: 1, text: 'Keep this transcript', timestamp: '00:00', speaker_label: 'You' },
      { id: 2, meetingId: 'old-recording', sequence_id: 2, text: 'Old final segment', timestamp: '00:01',
        audio_start_time: 1, audio_end_time: 2, duration: 1, speaker_label: 'Guest',
        candidate_speaker_ids: ['guest'], diarization_status: 'provisional' },
    ],
  });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: {
    folder_path: '/recordings/Old', meeting_name: 'Old meeting', transcript_warning: transcriptWarning,
  } });
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.listeners.get('recording-started')({ payload: {} });
  state.transcriptsRef.current = [{ id: 'new-segment', sequence_id: 1, text: 'New recording text' }];
  state.storage.set('indexeddb_current_meeting_id', 'new-recording');
  await state.listeners.get('recording-stopped')({ payload: {
    folder_path: '/recordings/New', meeting_name: 'New meeting',
  } });
  finishPoll({ is_processing: true, chunks_in_queue: 1 });
  await stopping;
  const [title, transcripts, folder] = state.calls.savedMeetings[0];
  assert.equal(title, 'Old meeting');
  assert.equal(folder, '/recordings/Old');
  assert.deepEqual(Array.from(transcripts, t => t.text), ['Keep this transcript', 'Old final segment']);
  assert.equal(transcripts[0].speaker_label, 'You');
  assert.equal(transcripts[1].audio_start_time, 1);
  assert.equal(transcripts[1].audio_end_time, 2);
  assert.equal(transcripts[1].speaker_label, 'Guest');
  assert.equal(transcripts[1].candidate_speaker_ids[0], 'guest');
  assert.equal(transcripts[1].diarization_status, 'provisional');
  assert.deepEqual(state.calls.recoveryReads, ['old-recording']);
  assert.deepEqual(state.calls.markedIds, ['old-recording']);
  assert.equal(state.calls.refreshes, 1);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.calls.navigation.length, 0);
  assert.equal(state.storage.get('indexeddb_current_meeting_id'), 'new-recording');
  assert.equal(state.storage.get('last_recording_folder_path'), '/recordings/New');
});

for (const delay of [4000, 500]) {
  test(`a new recording during the ${delay} ms wait does not skip the old save`, async t => {
    const state = await loadHook({ deferDelayMs: delay });
    t.after(state.unmount);
    const stopping = state.hook.handleRecordingStop(true);
    await new Promise(setImmediate);
    assert.equal(state.delayedWaits.length, 1);
    state.listeners.get('recording-started')({ payload: {} });
    state.transcriptsRef.current = [{ text: 'New recording text' }];
    state.storage.set('indexeddb_current_meeting_id', 'new-recording');
    state.delayedWaits[0]();
    await stopping;
    assert.equal(state.calls.save, 1);
    assert.equal(state.calls.savedMeetings[0][1][0].text, 'Keep this transcript');
    assert.deepEqual(state.calls.markedIds, ['old-recording']);
    assert.equal(state.calls.clear, 0);
    assert.equal(state.calls.navigation.length, 0);
  });
}

test('starting a new recording dismisses the old audio save-error notification', async t => {
  const state = await loadHook({ allowSuccess: true });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Previous audio save failed' } });
  assert.equal(state.activeToasts.has('recording-save-failed'), true);
  state.listeners.get('recording-started')({ payload: {} });
  await state.listeners.get('recording-stopped')({ payload: {} });
  await state.hook.handleRecordingStop(true);
  assert.equal(state.activeToasts.has('recording-save-failed'), false);
  assert.equal(state.toasts.at(-1)[0], 'Recording saved successfully!');
});

test('a recovery read failure keeps the original transcript snapshot for saving', async t => {
  let finishPoll;
  const state = await loadHook({
    poll: () => new Promise(resolve => { finishPoll = resolve; }),
    loadStored: async () => { throw new Error('IndexedDB read failed'); },
  });
  t.after(state.unmount);
  const stopping = state.hook.handleRecordingStop(true);
  await new Promise(setImmediate);
  state.transcriptsRef.current = [];
  state.listeners.get('recording-started')({ payload: {} });
  finishPoll({ is_processing: true, chunks_in_queue: 1 });
  await stopping;
  assert.equal(state.calls.save, 1);
  assert.equal(state.calls.savedMeetings[0][1][0].text, 'Keep this transcript');
});


test('a reported save failure releases only its own exit token after keeping recovery data', async t => {
  const state = await loadHook({ save: async () => { throw new Error('Database unavailable'); } });
  t.after(state.unmount);
  await state.listeners.get('recording-stopped')({ payload: { save_token: 'failed-token' } });
  assert.deepEqual(state.calls.saveAcks, []);
  await state.hook.handleRecordingStop(true);
  assert.deepEqual(state.calls.saveAcks, ['failed-token']);
  assert.equal(state.calls.markSaved, 0);
  assert.equal(state.calls.clear, 0);
  assert.equal(state.statuses.at(-1).status, 'ERROR');
});
