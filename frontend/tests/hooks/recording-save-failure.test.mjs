import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

function loadHook({ poll = async () => ({ is_processing: false, chunks_in_queue: 0 }) } = {}) {
  const listeners = new Map();
  const statuses = [];
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
    if (name === 'react') return {
      useEffect: fn => fn(), useCallback: fn => fn, useRef: value => ({ current: value }),
    };
    if (name === 'next/navigation') return { useRouter: () => ({ push() {} }) };
    if (name === '@tauri-apps/api/event') return { listen: async (event, callback) => {
      listeners.set(event, callback);
      return () => listeners.delete(event);
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
    sessionStorage: { setItem: (key, value) => storage.set(key, value) },
    setTimeout: fn => fn(),
  });
  const hook = module.exports.useRecordingStop(() => {}, () => {});
  return { hook, listeners, statuses, toasts, calls, storage };
}

test('a save error shows the recovery folder and keeps the transcript recovery copy', async () => {
  const state = loadHook();
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

test('a later successful stop clears the prior save error', async () => {
  const state = loadHook();
  await state.listeners.get('recording-stopped')({ payload: { save_error: 'Write failed' } });
  await state.listeners.get('recording-stopped')({ payload: { save_error: null } });
  await state.hook.handleRecordingStop(false);
  assert.equal(state.calls.poll, 1);
  assert.equal(state.statuses.at(-1).status, 'IDLE');
});

test('a save error that arrives during post-processing prevents a success result', async () => {
  let finishPoll;
  const state = loadHook({ poll: () => new Promise(resolve => { finishPoll = resolve; }) });
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
