import { useState, useEffect, useCallback, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { invoke } from '@tauri-apps/api/core';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useRecordingState, RecordingStatus } from '@/contexts/RecordingStateContext';
import { storageService } from '@/services/storageService';
import { indexedDBService } from '@/services/indexedDBService';
import type { Transcript } from '@/types';
import type { RecordingStoppedPayload } from '@/services/recordingService';
import { transcriptService } from '@/services/transcriptService';
import Analytics from '@/lib/analytics';
import {
  applyPinnedSummaryLanguageToMeeting,
  detectAndCacheSummaryLanguage,
} from '@/lib/summary-language-preferences';

interface RecordingStopSession {
  saveError: string | null;
  transcriptWarning: string | null;
  transcripts?: Transcript[];
  folderPath?: string | null;
  meetingName?: string | null;
  saveToken?: string;
}

async function completeRecordingSave(saveToken: string): Promise<void> {
  const toastId = `recording-save-status-${saveToken}`;
  try {
    await invoke('complete_recording_save', { saveToken });
    toast.dismiss(toastId);
  } catch (error) {
    console.error('Could not release recording save guard:', error);
    toast.error('Could not finish the recording save status', {
      id: toastId,
      duration: Infinity,
      dismissible: false,
      closeButton: false,
      description: 'Quit is still blocked. Retry to update the save status.',
      action: { label: 'Retry', onClick: event => {
        event.preventDefault();
        void completeRecordingSave(saveToken);
      } },
    });
  }
}

/** Merge transcript snapshots by sequence ID. Later copies win; ids stay stable. */
function mergeTranscripts(base: Transcript[], updates: Transcript[]): Transcript[] {
  const bySequence = new Map(base.map(t => [t.sequence_id ?? t.id, t]));
  for (const segment of updates) {
    const key = segment.sequence_id ?? segment.id;
    const prior = bySequence.get(key);
    bySequence.set(key, { ...prior, ...segment, id: prior?.id ?? segment.id });
  }
  return Array.from(bySequence.values()).sort((a, b) =>
    (a.chunk_start_time ?? a.audio_start_time ?? 0) - (b.chunk_start_time ?? b.audio_start_time ?? 0)
    || (a.sequence_id ?? 0) - (b.sequence_id ?? 0)
  );
}

/** Record a saved meeting, even after the page unmounts or a new recording starts. */
async function trackMeetingCompletion(meetingId: string, transcripts: Transcript[], transcriptComplete: boolean) {
  try {
    if (transcriptComplete) {
      Analytics.trackTranscriptionSuccess();
    }

    // Calculate meeting duration from transcript timestamps
    let durationSeconds = 0;
    if (transcripts.length > 0 && transcripts[0].audio_start_time !== undefined) {
      // Use audio_end_time of last transcript if available
      const lastTranscript = transcripts[transcripts.length - 1];
      durationSeconds = lastTranscript.audio_end_time || lastTranscript.audio_start_time || 0;
    }

    // Calculate word count
    const transcriptWordCount = transcripts
      .map(t => t.text.split(/\s+/).length)
      .reduce((a, b) => a + b, 0);

    // Calculate words per minute
    const wordsPerMinute = durationSeconds > 0 ? transcriptWordCount / (durationSeconds / 60) : 0;

    // Get meetings count today
    const meetingsToday = await Analytics.getMeetingsCountToday();

    // Track meeting completed
    await Analytics.trackMeetingCompleted(meetingId, {
      duration_seconds: durationSeconds,
      transcript_segments: transcripts.length,
      transcript_word_count: transcriptWordCount,
      words_per_minute: wordsPerMinute,
      meetings_today: meetingsToday
    });

    // Update meeting count in analytics.json
    await Analytics.updateMeetingCount();

    // Check for activation (first meeting)
    const { Store } = await import('@tauri-apps/plugin-store');
    const store = await Store.load('analytics.json');
    const totalMeetings = await store.get<number>('total_meetings');

    if (totalMeetings === 1) {
      const daysSinceInstall = await Analytics.calculateDaysSince('first_launch_date');
      await Analytics.track('user_activated', {
        meetings_count: '1',
        days_since_install: daysSinceInstall?.toString() || 'null',
        first_meeting_duration_seconds: durationSeconds.toString()
      });
    }
  } catch (analyticsError) {
    console.error('Failed to track meeting completion analytics:', analyticsError);
    // Don't block user flow on analytics errors
  }
}

type SummaryStatus = 'idle' | 'processing' | 'summarizing' | 'regenerating' | 'completed' | 'error';

interface UseRecordingStopReturn {
  handleRecordingStop: (callApi: boolean) => Promise<void>;
  isStopping: boolean;
  isProcessingTranscript: boolean;
  isSavingTranscript: boolean;
  summaryStatus: SummaryStatus;
  setIsStopping: (value: boolean) => void;
}

/**
 * Custom hook for managing recording stop lifecycle.
 * Handles the complex stop sequence: transcription wait → buffer flush → SQLite save → navigation.
 *
 * Features:
 * - Transcription completion polling (60s max, 500ms interval)
 * - Transcript buffer flush coordination
 * - SQLite meeting save with folder_path from sessionStorage
 * - Comprehensive analytics tracking (duration, word count, activation)
 * - Auto-navigation to meeting details
 * - Toast notifications for success/error
 * - Window exposure for Rust callbacks
 */
export function useRecordingStop(
  setIsRecording: (value: boolean) => void,
  setIsRecordingDisabled: (value: boolean) => void
): UseRecordingStopReturn {
  // USE global state instead
  const recordingState = useRecordingState();
  const {
    status,
    setStatus,
    isStopping,
    isProcessing: isProcessingTranscript,
    isSaving: isSavingTranscript
  } = recordingState;

  const {
    transcriptsRef,
    flushBuffer,
    clearTranscripts,
    meetingTitle,
    markMeetingAsSaved,
    currentMeetingId,
  } = useTranscripts();

  const {
    refetchMeetings,
    setCurrentMeeting,
    setMeetings,
    meetings,
    setIsMeetingActive,
  } = useSidebar();

  const router = useRouter();

  // A stop owns its session until a later recording starts.
  const stopSessionRef = useRef<RecordingStopSession>({ saveError: null, transcriptWarning: null });
  const stopInProgressRef = useRef<RecordingStopSession | null>(null);
  const pendingNavigationRef = useRef<RecordingStopSession | null>(null);
  // The page's Stop button calls handleRecordingStop only after stop_recording returns.
  const stopRequestedRef = useRef(false);
  const isMountedRef = useRef(true);
  const disposeListenersRef = useRef<(() => void) | null>(null);

  // Promise to track recording-stopped event data (fixes race condition with recording-stop-complete)
  const recordingStoppedDataRef = useRef<Promise<void> | null>(null);

  // Keep event subscriptions mounted while callbacks change between renders.
  const eventHandlersRef = useRef({ flushBuffer, setIsRecording, setIsRecordingDisabled, setIsMeetingActive, setStatus });
  useEffect(() => {
    eventHandlersRef.current = { flushBuffer, setIsRecording, setIsRecordingDisabled, setIsMeetingActive, setStatus };
  });

  // Set up recording lifecycle listeners once per mount.
  useEffect(() => {
    isMountedRef.current = true;
    const unlisteners: (() => void)[] = [];
    let cancelled = false;
    const retainUnlistener = (unlisten: () => void) => {
      if (cancelled) unlisten();
      else unlisteners.push(unlisten);
    };

    void listen('recording-started', () => {
      // Capture the old transcript before the new recording clears the context.
      if (transcriptsRef.current.length > 0) {
        stopSessionRef.current.transcripts = mergeTranscripts(
          stopSessionRef.current.transcripts ?? [],
          transcriptsRef.current,
        );
      }
      stopSessionRef.current = { saveError: null, transcriptWarning: null };
      // A requested stop that never reached the handler has ended by now.
      if (!isMountedRef.current && !stopInProgressRef.current) {
        stopRequestedRef.current = false;
        disposeListenersRef.current?.();
      }
      toast.dismiss('recording-transcript-incomplete');
      toast.dismiss('recording-save-failed');
      recordingStoppedDataRef.current = null;
      sessionStorage.removeItem('last_recording_folder_path');
      sessionStorage.removeItem('last_recording_meeting_name');
    }).then(retainUnlistener).catch(error => {
      console.error('Failed to setup recording started listener:', error);
    });

    const setupRecordingStoppedListener = async () => {
      try {
        console.log('Setting up recording-stopped listener for navigation...');
        const unlisten = await listen<RecordingStoppedPayload>('recording-stopped', async (event) => {
          // Create promise that resolves when sessionStorage is set (prevents race condition)
          recordingStoppedDataRef.current = (async () => {
            const { folder_path, meeting_name, save_error, transcript_warning, save_token } = event.payload;
            stopSessionRef.current.saveToken = save_token;
            stopSessionRef.current.saveError = save_error ?? null;
            stopSessionRef.current.transcriptWarning = transcript_warning ?? null;
            stopSessionRef.current.folderPath = folder_path ?? null;
            stopSessionRef.current.meetingName = meeting_name ?? null;
            if (transcript_warning && isMountedRef.current) {
              toast.warning('Transcript may be incomplete', {
                id: 'recording-transcript-incomplete',
                description: transcript_warning,
                duration: 20000,
              });
            } else if (!transcript_warning) {
              toast.dismiss('recording-transcript-incomplete');
            }
            if (save_error && isMountedRef.current) {
              const { flushBuffer, setIsRecording, setIsRecordingDisabled, setIsMeetingActive, setStatus } = eventHandlersRef.current;
              flushBuffer();
              setIsRecording(false);
              setIsRecordingDisabled(false);
              setIsMeetingActive(false);
              setStatus(RecordingStatus.ERROR, save_error);
              toast.error('Recording save failed', {
                id: 'recording-save-failed',
                description: save_error,
                duration: 20000,
              });
            }

            // Store folder_path and meeting_name for later use in handleRecordingStop
            if (folder_path) {
              sessionStorage.setItem('last_recording_folder_path', folder_path);
            }
            if (meeting_name) {
              sessionStorage.setItem('last_recording_meeting_name', meeting_name);
            }
          })();

        });
        retainUnlistener(unlisten);
        console.log('Recording stopped listener setup complete');
      } catch (error) {
        console.error('Failed to setup recording stopped listener:', error);
      }
    };

    setupRecordingStoppedListener();

    const disposeListeners = () => {
      cancelled = true;
      unlisteners.forEach(unlisten => unlisten());
      unlisteners.length = 0;
    };
    disposeListenersRef.current = disposeListeners;
    return () => {
      isMountedRef.current = false;
      // Keep session changes visible until stop work and delayed cleanup have finished.
      if (!stopInProgressRef.current && !stopRequestedRef.current && !pendingNavigationRef.current) disposeListeners();
    };
  }, []);

  // Main recording stop handler
  const handleRecordingStop = useCallback(async (isCallApi: boolean) => {
    // Keep this recording's data while a later recording takes over the UI.
    const stopSession = stopSessionRef.current;
    const isCurrent = () => stopSessionRef.current === stopSession;
    const recoveryMeetingId = currentMeetingId || sessionStorage.getItem('indexeddb_current_meeting_id');
    if (stopInProgressRef.current === stopSession) return;
    stopRequestedRef.current = false;
    stopSession.transcripts = [...transcriptsRef.current];
    const initialFolderPath = sessionStorage.getItem('last_recording_folder_path');
    const initialMeetingName = sessionStorage.getItem('last_recording_meeting_name');
    stopInProgressRef.current = stopSession;
    let unlistenComplete: (() => void) | undefined;
    let savedToSQLite = false;
    try {
      if (recordingStoppedDataRef.current) await recordingStoppedDataRef.current;

      if (isCurrent() && isMountedRef.current) {
        setStatus(RecordingStatus.STOPPING);
        setIsRecording(false);
        setIsRecordingDisabled(true);
      }
      const stopStartTime = Date.now();

      // Keep the transcript recovery copy after a failed file save.
      if (stopSession.saveError) {
        flushBuffer();
        if (isCurrent()) setIsMeetingActive(false);
        throw new Error(stopSession.saveError);
      }
      console.log('Post-stop processing (new implementation)...', {
        stop_initiated_at: new Date(stopStartTime).toISOString(),
        current_transcript_count: transcriptsRef.current.length
      });

      // Note: stop_recording is already called by RecordingControls.stopRecordingAction
      // This function only handles post-stop processing (transcription wait, API call, navigation)
      console.log('Recording already stopped by RecordingControls, processing transcription...');

      // Wait for transcription to complete
      if (isCurrent()) setStatus(RecordingStatus.PROCESSING_TRANSCRIPTS, 'Waiting for transcription...');
      console.log('Waiting for transcription to complete...');

      const MAX_WAIT_TIME = 60000; // 60 seconds maximum wait (increased for longer processing)
      const POLL_INTERVAL = 500; // Check every 500ms
      let elapsedTime = 0;
      let transcriptionComplete = false;

      // Listen for transcription-complete event
      unlistenComplete = await listen('transcription-complete', () => {
        console.log('Received transcription-complete event');
        transcriptionComplete = true;
      });

      // Poll for transcription status. A newer recording means Rust already drained this one.
      while (isCurrent() && elapsedTime < MAX_WAIT_TIME && !transcriptionComplete) {
        try {
          const status = await transcriptService.getTranscriptionStatus();
          if (!isCurrent()) break;
          console.log('Transcription status:', status);

          // Check if transcription is complete
          if (!status.is_processing && status.chunks_in_queue === 0) {
            console.log('Transcription complete - no active processing and no chunks in queue');
            transcriptionComplete = true;
            break;
          }

          // If no activity for more than 8 seconds and no chunks in queue, consider it done (increased from 5s to 8s)
          if (status.last_activity_ms > 8000 && status.chunks_in_queue === 0) {
            console.log('Transcription likely complete - no recent activity and empty queue');
            transcriptionComplete = true;
            break;
          }

          // Update user with current status
          if (status.chunks_in_queue > 0) {
            console.log(`Processing ${status.chunks_in_queue} remaining audio chunks...`);
            setStatus(RecordingStatus.PROCESSING_TRANSCRIPTS, `Processing ${status.chunks_in_queue} remaining chunks...`);
          }

          // Wait before next check
          await new Promise(resolve => setTimeout(resolve, POLL_INTERVAL));
          if (!isCurrent()) break;
          elapsedTime += POLL_INTERVAL;
        } catch (error) {
          console.error('Error checking transcription status:', error);
          break;
        }
      }

      // Clean up listener
      console.log('🧹 CLEANUP: Cleaning up transcription-complete listener');
      unlistenComplete();
      unlistenComplete = undefined;

      if (!transcriptionComplete && elapsedTime >= MAX_WAIT_TIME) {
        console.warn('⏰ Transcription wait timeout reached after', elapsedTime, 'ms');
      } else if (isCurrent()) {
        console.log('✅ Transcription completed after', elapsedTime, 'ms');
        // Wait longer for any late transcript segments (increased from 1s to 4s)
        console.log('⏳ Waiting for late transcript segments...');
        await new Promise(resolve => setTimeout(resolve, 4000));
      }

      // Refresh only this recording's UI buffer. A new recording has its own data.
      if (isCurrent()) {
        setStatus(RecordingStatus.PROCESSING_TRANSCRIPTS, 'Flushing transcript buffer...');
        flushBuffer();
        await new Promise(resolve => setTimeout(resolve, 500));
        if (isCurrent()) stopSession.transcripts = mergeTranscripts(stopSession.transcripts ?? [], transcriptsRef.current);
      }

      if (stopSession.saveError) {
        throw new Error(stopSession.saveError);
      }

      // Save to SQLite
      // NOTE: enabled to save COMPLETE transcripts after frontend receives all updates
      // This ensures user sees all transcripts streaming in before database save
      if (isCallApi && (transcriptionComplete || !isCurrent())) {

        if (isCurrent()) setStatus(RecordingStatus.SAVING, 'Saving meeting to database...');

        // IndexedDB can hold final segments that were still in the UI buffer when
        // the next recording started. Read only this recording's recovery ID.
        let storedTranscripts: Transcript[] = [];
        if (recoveryMeetingId) {
          try {
            const stored = await indexedDBService.getTranscripts(recoveryMeetingId);
            storedTranscripts = stored.map(({ id, meetingId: _meetingId, storedAt: _storedAt, sequenceId, ...segment }) => ({
              ...segment,
              id: id?.toString() || `${recoveryMeetingId}-${segment.sequence_id ?? sequenceId}`,
              sequence_id: segment.sequence_id ?? sequenceId,
            }));
          } catch (error) {
            console.warn('Could not read stored transcripts; keeping the stop snapshot:', error);
          }
        }
        const freshTranscripts = mergeTranscripts(stopSession.transcripts ?? [], storedTranscripts);
        if (stopSession.saveError) throw new Error(stopSession.saveError);

        const folderPath = stopSession.folderPath ?? initialFolderPath;
        const savedMeetingName = stopSession.meetingName ?? initialMeetingName;

        console.log('💾 Saving COMPLETE transcripts to database...', {
          transcript_count: freshTranscripts.length,
          meeting_name: savedMeetingName || meetingTitle,
          folder_path: folderPath,
          sample_text: freshTranscripts.length > 0 ? freshTranscripts[0].text.substring(0, 50) + '...' : 'none',
          last_transcript: freshTranscripts.length > 0 ? freshTranscripts[freshTranscripts.length - 1].text.substring(0, 30) + '...' : 'none',
        });

        try {
          const responseData = await storageService.saveMeeting(
            savedMeetingName || meetingTitle || 'New Meeting',  // PREFER savedMeetingName (backend source)
            freshTranscripts,
            folderPath
          );

          const meetingId = responseData.meeting_id;
          if (!meetingId) {
            console.error('No meeting_id in response:', responseData);
            throw new Error('No meeting ID received from save operation');
          }

          if (stopSession.saveError) throw new Error(stopSession.saveError);
          // SQLite is committed. Mark that recording even if a new one has started.
          await markMeetingAsSaved(recoveryMeetingId);
          savedToSQLite = true;
          // Record success only after the final transcript result is known.
          void trackMeetingCompletion(meetingId, freshTranscripts, !stopSession.transcriptWarning);

          let shouldDetectSummaryLanguage = false;
          try {
            shouldDetectSummaryLanguage = !(await applyPinnedSummaryLanguageToMeeting(meetingId));
          } catch (error) {
            console.warn('Failed to apply pinned summary language preference for new meeting:', error);
            if (isCurrent() && isMountedRef.current) toast.warning('Could not apply default summary language', {
              description: 'The meeting was saved, but the default summary language was not applied.',
            });
          }

          if (shouldDetectSummaryLanguage) {
            try {
              await detectAndCacheSummaryLanguage(
                meetingId,
                freshTranscripts.map(t => t.text)
              );
            } catch (error) {
              console.warn('Failed to detect summary language for new meeting:', error);
              if (isCurrent() && isMountedRef.current) toast.warning('Could not detect summary language', {
                description: 'The meeting was saved, but Auto could not detect the summary language.',
              });
            }
          }

          console.log('✅ Successfully saved COMPLETE meeting with ID:', meetingId);
          console.log('   Transcripts:', freshTranscripts.length);
          console.log('   folder_path:', folderPath);

          void (async () => {
            try {
              const settings = await invoke<{ enabled: boolean; postCallRefinementEnabled: boolean }>(
                'get_diarization_settings'
              );
              if (!settings.enabled || !settings.postCallRefinementEnabled || !folderPath) return;
              // Refinement needs Apple Silicon with macOS 14.4+; elsewhere keep the
              // live labels instead of recording a failed run.
              const models = await invoke<{ refinementSupported: boolean }>('get_diarization_model_status');
              if (models.refinementSupported) {
                await invoke('start_final_diarization', {
                  request: { meetingId, overwriteManualEdits: false }
                });
                await refetchMeetings();
              }
            } catch (error) {
              console.warn('Final speaker refinement kept the live transcript:', error);
            }
          })();

          await refetchMeetings();
          // Only the current mounted page may change shared UI or navigation.
          if (!isCurrent() || !isMountedRef.current) return;

          // Clean up session storage
          sessionStorage.removeItem('last_recording_folder_path');
          sessionStorage.removeItem('last_recording_meeting_name');

          try {
            const meetingData = await storageService.getMeeting(meetingId);
            if (!isCurrent() || !isMountedRef.current) return;
            if (meetingData) {
              setCurrentMeeting({
                id: meetingId,
                title: meetingData.title
              });
              console.log('✅ Current meeting set:', meetingData.title);
            }
          } catch (error) {
            if (!isCurrent() || !isMountedRef.current) return;
            console.warn('Could not fetch meeting details, using ID only:', error);
            setCurrentMeeting({ id: meetingId, title: savedMeetingName || meetingTitle || 'New Meeting' });
          }

          // Mark as completed
          setStatus(RecordingStatus.COMPLETED);

          // Keep the transcript warning visible after the meeting is saved.
          const showSavedToast = stopSession.transcriptWarning ? toast.warning : toast.success;
          showSavedToast(stopSession.transcriptWarning ? 'Meeting saved. Transcript may be incomplete' : 'Recording saved successfully!', {
            id: stopSession.transcriptWarning ? 'recording-transcript-incomplete' : undefined,
            description: stopSession.transcriptWarning ?? `${freshTranscripts.length} transcript segments saved.`,
            action: {
              label: 'View Meeting',
              onClick: () => {
                router.push(`/meeting-details?id=${meetingId}`);
                Analytics.trackButtonClick('view_meeting_from_toast', 'recording_complete');
              }
            },
            duration: stopSession.transcriptWarning ? 20000 : 10000,
          });

          // Auto-navigate after a short delay with source parameter
          pendingNavigationRef.current = stopSession;
          setTimeout(() => {
            try {
              if (!isCurrent()) return;
              // Navigate only from the page the user is still on.
              if (isMountedRef.current) {
                router.push(`/meeting-details?id=${meetingId}&source=recording`);
                Analytics.trackPageView('meeting_details');
              }
              clearTranscripts();

              // Reset to IDLE after navigation
              setStatus(RecordingStatus.IDLE);
            } finally {
              if (pendingNavigationRef.current === stopSession) pendingNavigationRef.current = null;
              if (!isMountedRef.current && !stopInProgressRef.current && !stopRequestedRef.current && !pendingNavigationRef.current) {
                disposeListenersRef.current?.();
              }
            }
          }, 2000);

        } catch (saveError) {
          const saveErrorMessage = saveError instanceof Error
            ? saveError.message
            : String(saveError || 'Unknown error');
          console.error('Failed to save meeting to database:', saveError);
          if (!isCurrent()) {
            // Leave the newer recording's state alone, but still report the lost save.
            toast.error('Failed to save the previous meeting', {
              description: `${saveErrorMessage} Its transcript is kept for recovery on the home page.`,
              duration: 20000,
            });
            return;
          }
          setStatus(RecordingStatus.ERROR, saveErrorMessage);
          toast.error('Failed to save meeting', {
            description: saveErrorMessage
          });
          throw saveError;
        }
      } else if (isCurrent()) {
        // No save needed, go back to IDLE.
        setStatus(RecordingStatus.IDLE);
      }

      if (!isCurrent()) return;
      setIsMeetingActive(false);
      // isRecording already set to false at function start
      if (isMountedRef.current) setIsRecordingDisabled(false);
    } catch (error) {
      console.error('Error in handleRecordingStop:', error);
      if (!isCurrent()) return;
      const errorMessage = error instanceof Error
        ? error.message
        : String(error || 'Unknown error');
      setStatus(RecordingStatus.ERROR, errorMessage);
      // isRecording already set to false at function start
      if (isMountedRef.current) setIsRecordingDisabled(false);
    } finally {
      // Release only this stop's token, after SQLite save or reported failure.
      if (stopSession.saveToken) await completeRecordingSave(stopSession.saveToken);
      unlistenComplete?.();
      // Navigation can unmount the page while the shared recording state lives on.
      if (!isMountedRef.current && isCurrent() && savedToSQLite) {
        clearTranscripts();
        setStatus(RecordingStatus.IDLE);
        setIsMeetingActive(false);
      }
      // An older stop must not release a newer stop's guard.
      if (stopInProgressRef.current === stopSession) {
        stopInProgressRef.current = null;
      }
      if (!isMountedRef.current && !stopInProgressRef.current && !pendingNavigationRef.current) {
        disposeListenersRef.current?.();
      }
    }
  }, [
    setIsRecording,
    setIsRecordingDisabled,
    setStatus,
    transcriptsRef,
    flushBuffer,
    clearTranscripts,
    meetingTitle,
    markMeetingAsSaved,
    currentMeetingId,
    refetchMeetings,
    setCurrentMeeting,
    setMeetings,
    meetings,
    setIsMeetingActive,
    router,
  ]);

  // Expose handleRecordingStop function to window for Rust callbacks
  const handleRecordingStopRef = useRef(handleRecordingStop);
  useEffect(() => {
    handleRecordingStopRef.current = handleRecordingStop;
  });

  useEffect(() => {
    (window as any).handleRecordingStop = (callApi: boolean = true) => {
      handleRecordingStopRef.current(callApi);
    };

    // Cleanup on unmount
    return () => {
      delete (window as any).handleRecordingStop;
    };
  }, []);

  // Derive summaryStatus from RecordingStatus for backward compatibility
  const summaryStatus: SummaryStatus = status === RecordingStatus.PROCESSING_TRANSCRIPTS ? 'processing' : 'idle';

  return {
    handleRecordingStop,
    isStopping,
    isProcessingTranscript,
    isSavingTranscript,
    summaryStatus,
    setIsStopping: (value: boolean) => {
      stopRequestedRef.current = value;
      if (value) {
        setStatus(RecordingStatus.STOPPING);
      } else if (!isMountedRef.current && !stopInProgressRef.current && !pendingNavigationRef.current) {
        // A duplicate or inactive stop does not start post-processing.
        disposeListenersRef.current?.();
      }
    },
  };
}
