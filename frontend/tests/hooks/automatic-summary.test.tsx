import React from 'react';
import { afterAll, afterEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
const originalCore = { ...await import('@tauri-apps/api/core') };
let status: string | null = null;
mock.module('@tauri-apps/api/core', () => ({ ...originalCore, invoke: async () => status ? { currentStatus: status } : null }));
const { runRecordingSpeakerAnalysis, waitForRecordingSpeakerAnalysis } = await import('../../src/lib/recording-speaker-analysis');
const { useAutomaticSummary } = await import('../../src/hooks/meeting-details/useAutomaticSummary');
afterAll(() => mock.module('@tauri-apps/api/core', () => originalCore));
let renderer: ReactTestRenderer | undefined;
afterEach(async () => { if (renderer) await act(async () => renderer!.unmount()); renderer = undefined; status = null; });
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(yes => { resolve = yes; });
  return { promise, resolve };
}
let current: ReturnType<typeof useAutomaticSummary>;
let generated: string[];
let enabled: boolean;
let meetingId: string;
let speaker: string;
function View() {
  current = useAutomaticSummary({ meetingId, enabled, generate: async () => { generated.push(`${meetingId}:${speaker}`); } });
  return null;
}
async function show() {
  await act(async () => { if (renderer) renderer.update(<View />); else renderer = create(<View />); });
}
function reset() { generated = []; enabled = true; meetingId = 'meeting-a'; speaker = 'provisional'; }

test('automatic generation waits through startup and reads the final speaker', async () => {
  reset();
  const analysis = deferred();
  const task = runRecordingSpeakerAnalysis(meetingId, async () => { await analysis.promise; speaker = 'Alex'; });
  await show();
  expect(current.waiting).toBe(true);
  expect(generated).toEqual([]);
  await act(async () => { analysis.resolve(); await task; });
  expect(generated).toEqual(['meeting-a:Alex']);
  expect(current.waiting).toBe(false);
  await show();
  expect(generated).toHaveLength(1);
});

test('disabled auto-summary does not wait or generate', async () => {
  reset(); enabled = false;
  await show();
  expect(current.waiting).toBe(false);
  expect(generated).toEqual([]);
});

test('failed analysis permits a summary from the current transcript', async () => {
  reset();
  const analysis = deferred();
  const task = runRecordingSpeakerAnalysis(meetingId, async () => { await analysis.promise; throw new Error('analysis failed'); });
  void task.catch(() => {});
  await show();
  await act(async () => { analysis.resolve(); await task.catch(() => {}); });
  expect(generated).toEqual(['meeting-a:provisional']);
});

test('manual generation cancels a pending automatic attempt', async () => {
  reset();
  const analysis = deferred();
  const task = runRecordingSpeakerAnalysis(meetingId, () => analysis.promise);
  await show();
  await act(async () => current.cancel());
  await act(async () => { analysis.resolve(); await task; });
  expect(current.waiting).toBe(false);
  expect(generated).toEqual([]);
  await show();
  expect(generated).toEqual([]);
});

test('leaving a meeting prevents its delayed automatic summary', async () => {
  reset();
  const analysis = deferred();
  const task = runRecordingSpeakerAnalysis(meetingId, () => analysis.promise);
  await show();
  meetingId = 'meeting-b';
  await show();
  expect(generated).toEqual(['meeting-b:provisional']);
  await act(async () => { analysis.resolve(); await task; });
  expect(generated).toEqual(['meeting-b:provisional']);
});

test('an analysis that survives reload is polled until it finishes', async () => {
  reset(); status = 'processing';
  const result = waitForRecordingSpeakerAnalysis('reloaded', new AbortController().signal);
  status = 'completed';
  expect(await result).toBe('ready');
});

test('waiting is bounded even if native analysis stalls', async () => {
  const analysis = deferred();
  const task = runRecordingSpeakerAnalysis('slow', () => analysis.promise);
  expect(await waitForRecordingSpeakerAnalysis('slow', new AbortController().signal, 1)).toBe('timeout');
  analysis.resolve(); await task;
});
for (const terminal of [null, 'completed', 'failed', 'fallback_to_live', 'cancelled']) {
  test(`inactive or finished analysis (${terminal}) permits immediate generation`, async () => {
    reset(); status = terminal;
    await show();
    expect(generated).toEqual(['meeting-a:provisional']);
  });
}
