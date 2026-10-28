import { invoke } from '@tauri-apps/api/core';

const pendingAnalysis = new Map<string, Promise<void>>();

/** Register before checking settings, so navigation cannot overtake startup. */
export function runRecordingSpeakerAnalysis(meetingId: string, work: () => Promise<void>): Promise<void> {
  const task = Promise.resolve().then(work);
  pendingAnalysis.set(meetingId, task);
  const cleanup = () => { if (pendingAnalysis.get(meetingId) === task) pendingAnalysis.delete(meetingId); };
  void task.then(cleanup, cleanup);
  return task;
}

/** A reload loses the local promise, but the native analysis keeps its status. */
export function waitForRecordingSpeakerAnalysis(
  meetingId: string,
  signal: AbortSignal,
  timeoutMs = 10 * 60 * 1000,
): Promise<'ready' | 'timeout' | 'cancelled'> {
  return new Promise(resolve => {
    let finished = false;
    let pollTimer: ReturnType<typeof setTimeout> | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const finish = (outcome: 'ready' | 'timeout' | 'cancelled') => {
      if (finished) return;
      finished = true;
      clearTimeout(pollTimer);
      clearTimeout(timeout);
      signal.removeEventListener('abort', cancelled);
      resolve(outcome);
    };
    const cancelled = () => finish('cancelled');
    if (signal.aborted) { finish('cancelled'); return; }
    signal.addEventListener('abort', cancelled, { once: true });
    timeout = setTimeout(() => finish('timeout'), timeoutMs);
    const inspect = async () => {
      if (finished) return;
      try {
        const task = pendingAnalysis.get(meetingId);
        if (task) {
          await task;
          finish('ready');
          return;
        }
        const state = await invoke<{ currentStatus: string } | null>('get_meeting_diarization_status', { meetingId });
        if (finished) return;
        if (state && ['processing', 'preparing_models', 'rebuilding_windows', 'saving'].includes(state.currentStatus)) {
          pollTimer = setTimeout(() => { void inspect(); }, 1000);
        } else {
          finish('ready');
        }
      } catch (error) {
        console.warn('Speaker analysis did not complete; using the current transcript:', error);
        finish('ready');
      }
    };
    void inspect();
  });
}
