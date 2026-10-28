import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
const originalCore = { ...await import('@tauri-apps/api/core') };
const originalEvents = { ...await import('@tauri-apps/api/event') };
const originalNavigation = { ...await import('next/navigation') };
const originalAnalytics = { ...await import('../../src/lib/analytics') };
const originalToast = { ...await import('sonner') };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
let work: ReturnType<typeof deferred<{ segments_count: number; duration_seconds: number }>>;
let progress: (event: { payload: any }) => void;
let commands: string[];
let cancelCommand: () => Promise<void>;
let notices: { kind: string; text: string; options: any }[];
let removals = 0;
let deferRegistration = false;
let releaseRegistration: (() => void) | undefined;
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke: async (command: string) => {
  commands.push(command);
  if (command === 'start_retranscription_command') return work.promise;
  if (command === 'cancel_retranscription_command') return cancelCommand();
  throw new Error(`Unexpected command: ${command}`);
} }));
mock.module('@tauri-apps/api/event', () => ({ ...originalEvents, listen: async (_event: string, callback: typeof progress) => {
  progress = callback;
  const cleanup = () => { removals++; };
  if (deferRegistration) return new Promise<() => void>(resolve => { releaseRegistration = () => resolve(cleanup); });
  return cleanup;
} }));
mock.module('next/navigation', () => ({ ...originalNavigation, useRouter: () => ({ push() {} }) }));
mock.module('../../src/lib/analytics', () => ({ default: { track: async () => {}, trackError: async () => {} } }));
mock.module('sonner', () => ({ toast: Object.fromEntries(['loading', 'success', 'error', 'info'].map(kind => [kind,
  (text: string, options: unknown) => notices.push({ kind, text, options })])) }));
const { TranscriptEnhancementProvider, useTranscriptEnhancement, useEnhancementCompletion } = await import('../../src/contexts/TranscriptEnhancementContext');
let current: ReturnType<typeof useTranscriptEnhancement>;
let pageMeeting: string | null;
let refreshed: string[];
function Controls() { current = useTranscriptEnhancement(); return null; }
function Page() { const meeting = pageMeeting; useEnhancementCompletion(meeting, () => { refreshed.push(meeting!); }); return null; }
let renderer: ReactTestRenderer | undefined;
const request = { meetingId: 'meeting-a', meetingFolderPath: '/recording', language: null, model: 'test', provider: 'whisper', assignSpeakers: false };
async function render(showPage = true) {
  await act(async () => {
    const view = <TranscriptEnhancementProvider><Controls />{showPage && <Page key={pageMeeting} />}</TranscriptEnhancementProvider>;
    if (renderer) renderer.update(view); else renderer = create(view);
  });
}
beforeEach(() => {
  work = deferred(); commands = []; notices = []; refreshed = []; pageMeeting = 'meeting-a';
  cancelCommand = async () => {}; removals = 0; deferRegistration = false; releaseRegistration = undefined;
});
afterEach(async () => { if (renderer) await act(async () => renderer!.unmount()); renderer = undefined; });
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvents);
  mock.module('next/navigation', () => originalNavigation);
  mock.module('../../src/lib/analytics', () => originalAnalytics);
  mock.module('sonner', () => originalToast);
});

test('enhancement survives page unmount and does not refresh another meeting', async () => {
  await render();
  await act(async () => current.start(request));
  await render(false);
  expect(current.job?.request.meetingId).toBe('meeting-a');
  await act(async () => progress({ payload: { meeting_id: 'meeting-a', message: 'Working', progress_percentage: 50 } }));
  expect(current.job?.progress?.progress_percentage).toBe(50);
  pageMeeting = 'meeting-b'; await render();
  await act(async () => { work.resolve({ segments_count: 4, duration_seconds: 10 }); await work.promise; });
  expect(current.job).toBe(null);
  expect(refreshed).toEqual([]);
  expect(notices.at(-1)?.kind).toBe('success');
  pageMeeting = 'meeting-a'; await render();
  expect(refreshed).toEqual(['meeting-a']);
});

test('completion refreshes the open meeting once and ignores late progress', async () => {
  await render(); await act(async () => current.start(request));
  await act(async () => { work.resolve({ segments_count: 4, duration_seconds: 10 }); await work.promise; });
  expect(refreshed).toEqual(['meeting-a']);
  const count = notices.length;
  await act(async () => progress({ payload: { meeting_id: 'meeting-a', message: 'Late' } }));
  expect(notices.length).toBe(count);
  await render(); expect(refreshed).toEqual(['meeting-a']);
});

test('startup failure without any event is reported and releases the job', async () => {
  await render(); await act(async () => current.start(request));
  await act(async () => { work.reject(new Error('Meeting is locked')); await work.promise.catch(() => {}); });
  expect(current.job).toBe(null);
  expect(notices.at(-1)?.kind).toBe('error');
  expect(notices.at(-1)?.options.description).toContain('Meeting is locked');
  expect(refreshed).toEqual([]);
});

test('only one job starts and cancellation waits for native work to finish', async () => {
  await render(); await act(async () => current.start(request));
  expect(() => current.start({ ...request, meetingId: 'meeting-b' })).toThrow('Another meeting');
  expect(commands.filter(command => command === 'start_retranscription_command')).toHaveLength(1);
  await act(async () => current.cancel());
  expect(current.job?.cancelling).toBe(true);
  expect(refreshed).toEqual([]);
  await act(async () => { work.reject(new Error('cancelled')); await work.promise.catch(() => {}); });
  expect(current.job).toBe(null);
  expect(notices.at(-1)?.kind).toBe('info');
});

test('failed cancellation keeps progress tracking and permits a retry', async () => {
  await render(); await act(async () => current.start(request));
  cancelCommand = async () => { throw new Error('Unavailable'); };
  await act(async () => current.cancel());
  expect(current.job?.cancelling).toBe(false);
  expect(notices.at(-1)?.kind).toBe('error');
  await act(async () => progress({ payload: { meeting_id: 'meeting-a', message: 'Still working' } }));
  expect(current.job?.progress?.message).toBe('Still working');
  await act(async () => { work.resolve({ segments_count: 4, duration_seconds: 10 }); await work.promise; });
});

test('late listener registration is removed after provider unmount', async () => {
  deferRegistration = true; await render();
  await act(async () => renderer!.unmount()); renderer = undefined;
  await act(async () => releaseRegistration!());
  expect(removals).toBe(1);
});
