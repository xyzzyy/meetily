'use client';

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, ReactNode } from 'react';
import { useRouter } from 'next/navigation';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { toast } from 'sonner';

export interface EnhancementProgress {
  meeting_id: string;
  stage: string;
  progress_percentage: number;
  message: string;
}

/** The backend runs one enhancement at a time. A failed run stays until its popup shows the error. */
export interface EnhancementRun {
  meetingId: string;
  status: 'running' | 'failed';
  progress: EnhancementProgress | null;
  error: string | null;
}

export interface EnhancementDialogState {
  meetingId: string;
  meetingFolderPath: string | null;
  open: boolean;
}

interface EnhancementRunContextType {
  run: EnhancementRun | null;
  dialog: EnhancementDialogState | null;
  openEnhancement: (meetingId: string, meetingFolderPath?: string | null) => void;
  setDialogOpen: (open: boolean) => void;
  markStarted: (meetingId: string) => void;
  markStartFailed: (meetingId: string) => void;
  dismissFailure: (meetingId: string) => void;
  /** Resolves to whether the backend accepted the cancel request. */
  cancelEnhancement: () => Promise<boolean>;
}

const EnhancementRunContext = createContext<EnhancementRunContextType | null>(null);

export const useEnhancementRun = () => {
  const ctx = useContext(EnhancementRunContext);
  if (!ctx) throw new Error('useEnhancementRun must be used within EnhancementRunProvider');
  return ctx;
};

export function EnhancementRunProvider({ children }: { children: ReactNode }) {
  const router = useRouter();
  const [run, setRun] = useState<EnhancementRun | null>(null);
  const [dialog, setDialog] = useState<EnhancementDialogState | null>(null);
  const dialogRef = useRef(dialog);
  useEffect(() => { dialogRef.current = dialog; }, [dialog]);
  const folderPaths = useRef(new Map<string, string | null>());
  // Events from a cancelled run still arrive while it stops; they must not reopen the meter.
  const cancelledMeetingRef = useRef<string | null>(null);

  const openEnhancement = useCallback((meetingId: string, meetingFolderPath?: string | null) => {
    if (meetingFolderPath !== undefined) folderPaths.current.set(meetingId, meetingFolderPath);
    setDialog({ meetingId, meetingFolderPath: folderPaths.current.get(meetingId) ?? null, open: true });
  }, []);

  const setDialogOpen = useCallback((open: boolean) => {
    setDialog(current => current && { ...current, open });
  }, []);

  const viewMeeting = useCallback((meetingId: string) => {
    const onMeeting = window.location.pathname.startsWith('/meeting-details')
      && new URLSearchParams(window.location.search).get('id') === meetingId;
    if (!onMeeting) router.push(`/meeting-details?id=${meetingId}`);
    openEnhancement(meetingId);
  }, [openEnhancement, router]);
  const viewMeetingRef = useRef(viewMeeting);
  useEffect(() => { viewMeetingRef.current = viewMeeting; }, [viewMeeting]);

  const markStarted = useCallback((meetingId: string) => {
    setRun({ meetingId, status: 'running', progress: null, error: null });
  }, []);

  const markStartFailed = useCallback((meetingId: string) => {
    setRun(current => current?.meetingId === meetingId ? null : current);
  }, []);

  const dismissFailure = useCallback((meetingId: string) => {
    setRun(current => current?.meetingId === meetingId && current.status === 'failed' ? null : current);
  }, []);

  const cancelEnhancement = useCallback(async () => {
    const meetingId = run?.status === 'running' ? run.meetingId : null;
    if (!meetingId) return false;
    cancelledMeetingRef.current = meetingId;
    setRun(null);
    try {
      await invoke('cancel_retranscription_command');
      return true;
    } catch (error) {
      cancelledMeetingRef.current = null;
      console.error('Failed to cancel retranscription:', error);
      return false;
    }
  }, [run]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    const popupShows = (meetingId: string) =>
      dialogRef.current?.open === true && dialogRef.current.meetingId === meetingId;
    const finishCancelled = (meetingId: string) => {
      if (cancelledMeetingRef.current !== meetingId) return false;
      cancelledMeetingRef.current = null;
      return true;
    };

    const setup = async () => {
      const handlers = [
        listen<EnhancementProgress>('retranscription-progress', event => {
          const progress = event.payload;
          if (cancelledMeetingRef.current === progress.meeting_id) return;
          // Also picks up a run the app did not start, such as after a window reload.
          setRun({ meetingId: progress.meeting_id, status: 'running', progress, error: null });
        }),
        listen<{ meeting_id: string; warning?: string | null }>('retranscription-complete', event => {
          const { meeting_id: meetingId, warning } = event.payload;
          if (finishCancelled(meetingId)) return;
          setRun(current => current?.meetingId === meetingId ? null : current);
          if (popupShows(meetingId)) return;
          toast.success('Enhancement finished', {
            description: warning || 'The enhanced transcript was applied. Undo is available.',
            action: { label: 'View', onClick: () => viewMeetingRef.current(meetingId) },
          });
        }),
        listen<{ meeting_id: string; error: string }>('retranscription-error', event => {
          const { meeting_id: meetingId, error } = event.payload;
          if (finishCancelled(meetingId)) {
            setRun(current => current?.meetingId === meetingId ? null : current);
            return;
          }
          // An open popup shows the error itself, so only a minimized run keeps it.
          if (popupShows(meetingId)) {
            setRun(current => current?.meetingId === meetingId ? null : current);
            return;
          }
          setRun({ meetingId, status: 'failed', progress: null, error });
          toast.error('Enhancement failed', {
            description: error,
            action: { label: 'View', onClick: () => viewMeetingRef.current(meetingId) },
          });
        }),
      ];
      for (const handler of handlers) {
        const unlisten = await handler;
        if (disposed) unlisten(); else unlisteners.push(unlisten);
      }
    };
    void setup();

    return () => {
      disposed = true;
      unlisteners.forEach(unlisten => unlisten());
    };
  }, []);

  const value = useMemo(() => ({
    run, dialog, openEnhancement, setDialogOpen, markStarted, markStartFailed, dismissFailure, cancelEnhancement,
  }), [run, dialog, openEnhancement, setDialogOpen, markStarted, markStartFailed, dismissFailure, cancelEnhancement]);

  return (
    <EnhancementRunContext.Provider value={value}>
      {children}
    </EnhancementRunContext.Provider>
  );
}
