'use client';

import React, { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useRouter } from 'next/navigation';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';

export interface EnhancementRequest {
  meetingId: string;
  meetingFolderPath: string;
  language: string | null;
  model: string;
  provider: string;
  assignSpeakers: boolean;
}
interface Progress {
  meeting_id: string;
  stage: string;
  progress_percentage: number;
  message: string;
}
interface Job { request: EnhancementRequest; token: object; cancelling: boolean; progress: Progress | null }
interface EnhancementContext {
  job: Job | null;
  completion: { meetingId: string; token: object } | null;
  start: (request: EnhancementRequest) => void;
  cancel: () => Promise<void>;
}
const Context = createContext<EnhancementContext | null>(null);
const toastId = 'transcript-enhancement';

export function TranscriptEnhancementProvider({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const routerRef = useRef(router);
  routerRef.current = router;
  const [job, setJob] = useState<Job | null>(null);
  const [completion, setCompletion] = useState<EnhancementContext['completion']>(null);
  const active = useRef<Job | null>(null);
  const mounted = useRef(true);
  const cancelRef = useRef<() => Promise<void>>(async () => {});
  const showProgress = useCallback((current: Job) => {
    toast.loading(current.cancelling ? 'Cancelling enhancement...' : 'Enhancing meeting', {
      id: toastId, duration: Infinity,
      description: current.progress?.message || 'You can continue to use the app.',
      action: { label: 'View meeting', onClick: () => routerRef.current.push(`/meeting-details?id=${encodeURIComponent(current.request.meetingId)}`) },
      cancel: current.cancelling ? undefined : { label: 'Cancel', onClick: event => {
        event.preventDefault(); void cancelRef.current();
      } },
    });
  }, []);

  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    let cleanup: (() => void) | undefined;
    void listen<Progress>('retranscription-progress', event => {
      const current = active.current;
      if (disposed || !current || current.request.meetingId !== event.payload.meeting_id) return;
      const next = { ...current, progress: event.payload };
      active.current = next; setJob(next); showProgress(next);
    }).then(unlisten => { if (disposed) unlisten(); else cleanup = unlisten; })
      .catch(error => console.warn('Could not listen for enhancement progress:', error));
    return () => { disposed = true; mounted.current = false; cleanup?.(); };
  }, [showProgress]);

  const cancel = useCallback(async () => {
    const current = active.current;
    if (!current || current.cancelling) return;
    const next = { ...current, cancelling: true };
    active.current = next; setJob(next); showProgress(next);
    try {
      await invoke('cancel_retranscription_command');
    } catch (error) {
      if (!mounted.current || active.current?.token !== current.token) return;
      const restored = { ...active.current, cancelling: false };
      active.current = restored; setJob(restored);
      showProgress(restored);
      toast.error('Could not cancel enhancement', { description: String(error) });
    }
  }, [showProgress]);
  cancelRef.current = cancel;

  const start = useCallback((request: EnhancementRequest) => {
    if (active.current) throw new Error('Another meeting is being enhanced. Wait for it to finish.');
    const current: Job = { request, token: {}, cancelling: false, progress: null };
    active.current = current; setJob(current); showProgress(current);
    void Analytics.track('enhance_transcript_started', {
      language: request.language || 'auto', model_provider: request.provider, model_name: request.model,
    }).catch(error => console.warn('Could not track enhancement:', error));
    void (async () => {
      try {
        const result = await invoke<{ segments_count: number; duration_seconds: number }>('start_retranscription_command', { ...request });
        if (!mounted.current || active.current?.token !== current.token) return;
        active.current = null; setJob(null);
        setCompletion({ meetingId: request.meetingId, token: current.token });
        toast.success(`Enhancement complete. ${result.segments_count} segments created.`, { id: toastId, cancel: undefined, action: { label: 'View meeting', onClick: () => routerRef.current.push(`/meeting-details?id=${encodeURIComponent(request.meetingId)}`) } });
        void Analytics.track('enhance_transcript_completed', {
          success: 'true', segments_count: String(result.segments_count), duration_seconds: String(result.duration_seconds),
        }).catch(() => {});
      } catch (error) {
        if (!mounted.current || active.current?.token !== current.token) return;
        const cancelled = active.current.cancelling;
        active.current = null; setJob(null);
        if (cancelled) toast.info('Enhancement cancelled', { id: toastId });
        else toast.error('Enhancement failed', { id: toastId, cancel: undefined, description: String(error), action: { label: 'View meeting', onClick: () => routerRef.current.push(`/meeting-details?id=${encodeURIComponent(request.meetingId)}`) } });
        void Analytics.trackError('enhance_transcript_failed', String(error)).catch(() => {});
      }
    })();
  }, [showProgress]);

  return <Context.Provider value={{ job, completion, start, cancel }}>{children}</Context.Provider>;
}

export function useTranscriptEnhancement() {
  const context = useContext(Context);
  if (!context) throw new Error('Transcript enhancement requires its provider');
  return context;
}

/** Refresh only the meeting currently shown, even if enhancement finishes elsewhere. */
export function useEnhancementCompletion(meetingId: string | null | undefined, refresh: () => Promise<void> | void) {
  const completion = useContext(Context)?.completion;
  const latest = useRef(refresh);
  latest.current = refresh;
  useEffect(() => {
    if (completion && completion.meetingId === meetingId) {
      const refresh = latest.current;
      void Promise.resolve().then(refresh).catch(error => console.error('Could not refresh enhanced meeting:', error));
    }
  }, [completion, meetingId]);
}
