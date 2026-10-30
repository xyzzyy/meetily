import { useCallback, useEffect, useRef, useState } from 'react';
import { waitForRecordingSpeakerAnalysis } from '@/lib/recording-speaker-analysis';

/** Automatic generation waits; manual generation remains available. */
export function useAutomaticSummary({ meetingId, enabled, generate, onStarted }: {
  meetingId: string;
  enabled: boolean;
  generate: () => Promise<void>;
  onStarted?: () => void;
}) {
  const started = useRef(new Set<string>());
  const activeWait = useRef<AbortController | null>(null);
  const latest = useRef({ generate, onStarted });
  latest.current = { generate, onStarted };
  const [waitingMeetingId, setWaitingMeetingId] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled || started.current.has(meetingId)) return;
    const controller = new AbortController();
    activeWait.current = controller;
    setWaitingMeetingId(meetingId);
    void (async () => {
      const outcome = await waitForRecordingSpeakerAnalysis(meetingId, controller.signal);
      if (controller.signal.aborted || outcome === 'cancelled') return;
      setWaitingMeetingId(null);
      started.current.add(meetingId);
      latest.current.onStarted?.();
      await latest.current.generate();
    })().catch(error => console.error('Automatic summary failed:', error));
    return () => controller.abort();
  }, [enabled, meetingId]);

  const cancel = useCallback(() => {
    started.current.add(meetingId);
    activeWait.current?.abort();
    setWaitingMeetingId(null);
  }, [meetingId]);
  return { waiting: enabled && waitingMeetingId === meetingId, cancel };
}
