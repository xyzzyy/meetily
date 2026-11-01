import React, { useState, useEffect, useRef, useMemo } from 'react';
import { RefreshCw, Globe, Loader2, AlertCircle, CheckCircle2, X, Cpu } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { Switch } from '../ui/switch';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '../ui/select';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { useConfig } from '@/contexts/ConfigContext';
import { useSpeakerRefinementAvailability } from '@/hooks/useSpeakerRefinementAvailability';
import { LANGUAGES } from '@/constants/languages';
import { languagesForProvider } from '@/lib/funasr';
import { useTranscriptionModels, ModelOption } from '@/hooks/useTranscriptionModels';
import { useEnhancementRun } from '@/contexts/EnhancementRunContext';
import { compareTranscriptText } from '@/lib/transcript-comparison';

interface RetranscribeDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  meetingFolderPath: string | null;
}

interface RetranscriptionResult {
  meeting_id: string;
  segments_count: number;
  duration_seconds: number;
  language: string | null;
  warning?: string | null;
}

interface EnhancementReview {
  id: string;
  original: { transcript: string }[];
  proposed: { text: string }[];
  warnings: string[];
  model?: string;
  applied?: boolean;
}

interface RetranscriptionError {
  meeting_id: string;
  error: string;
}

export function RetranscribeDialog({
  open,
  onOpenChange,
  meetingId,
  meetingFolderPath,
}: RetranscribeDialogProps) {
  const { selectedLanguage, transcriptModelConfig, diarizationSettings, isLoadingDiarizationSettings } = useConfig();
  const { supported: refinementSupported, checking: checkingRefinement, unavailableReason } = useSpeakerRefinementAvailability();
  const speakerLabelsEnabled = !isLoadingDiarizationSettings && diarizationSettings.enabled;
  const { run, markStarted, markStartFailed, dismissFailure, cancelEnhancement } = useEnhancementRun();
  // The run continues in the background when the popup is minimized.
  const isProcessing = run?.meetingId === meetingId && run.status === 'running';
  const progress = isProcessing ? run.progress : null;
  const otherMeetingRunning = run?.status === 'running' && run.meetingId !== meetingId;
  const failedRunError = run?.meetingId === meetingId && run.status === 'failed' ? run.error : null;
  const [review, setReview] = useState<EnhancementReview | null>(null);
  const [isApplying, setIsApplying] = useState(false);
  const [loadingReview, setLoadingReview] = useState(false);
  const comparison = useMemo(() => review ? compareTranscriptText(
    review.original.map(row => row.transcript).join('\n\n'),
    review.proposed.map(row => row.text).join('\n\n')) : null, [review]);
  const [error, setError] = useState<string | null>(null);
  const [selectedLang, setSelectedLang] = useState(selectedLanguage || 'auto');

  // Use centralized model fetching hook
  const {
    availableModels,
    selectedModelKey,
    setSelectedModelKey,
    loadingModels,
    modelNotice,
    fetchModels,
    resetSelection,
  } = useTranscriptionModels(transcriptModelConfig);

  // Stable refs for callbacks to avoid listener re-registration
  const onOpenChangeRef = useRef(onOpenChange);
  useEffect(() => { onOpenChangeRef.current = onOpenChange; }, [onOpenChange]);

  // Track previous open state to only reset on closed→open transition
  const prevOpenRef = useRef(false);

  // Helper to get selected model details (memoized)
  const selectedModelDetails = useMemo((): ModelOption | undefined => {
    if (!selectedModelKey) return undefined;
    const colonIndex = selectedModelKey.indexOf(':');
    if (colonIndex === -1) return undefined;
    const provider = selectedModelKey.slice(0, colonIndex);
    const name = selectedModelKey.slice(colonIndex + 1);
    return availableModels.find(m => m.provider === provider && m.name === name);
  }, [selectedModelKey, availableModels]);
  const isParakeetModel = selectedModelDetails?.provider === 'parakeet';
  // Nemotron finds speakers itself, so it does not need the speaker refinement model.
  const modelFindsSpeakers = selectedModelDetails?.provider === 'nemotron';
  const canAssignSpeakers = speakerLabelsEnabled && (refinementSupported || modelFindsSpeakers);
  const [assignSpeakers, setAssignSpeakers] = useState(canAssignSpeakers);
  const languageOptions = useMemo(
    () => languagesForProvider(selectedModelDetails?.provider, LANGUAGES),
    [selectedModelDetails?.provider],
  );

  // FUN-ASR supports fewer languages; fall back to auto-detect for any other.
  useEffect(() => {
    if (!languageOptions.some((language) => language.code === selectedLang)) {
      setSelectedLang('auto');
    }
  }, [languageOptions, selectedLang]);

  useEffect(() => {
    if (isParakeetModel && selectedLang !== 'auto') {
      setSelectedLang('auto');
    }
  }, [isParakeetModel, selectedLang]);

  // Reset state only when dialog transitions from closed to open
  // This prevents re-initialization when config changes while dialog is already open
  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;

    if (open && !wasOpen) {
      resetSelection();
      // Show why a run that failed while minimized stopped.
      setError(failedRunError);
      if (failedRunError) dismissFailure(meetingId);
      setSelectedLang(selectedLanguage || 'auto');

      // Fetch available models using centralized hook
      fetchModels();
    }
  }, [open, selectedLanguage, transcriptModelConfig, fetchModels]);

  // Default to the app-wide speaker labels setting, including when it finishes
  // loading after the dialog is already open.
  useEffect(() => {
    if (open) setAssignSpeakers(canAssignSpeakers);
  }, [open, canAssignSpeakers]);

  useEffect(() => {
    if (!open) return;
    // Reopened mid-run: the saved review is from an earlier run. Completion loads the new one.
    if (isProcessing) { setReview(null); return; }
    let active = true;
    setLoadingReview(true);
    setReview(null);
    invoke<EnhancementReview | null>('get_enhancement_review', { meetingId })
      .then(value => { if (active) setReview(value); })
      .catch(error => { if (active) setError(String(error)); })
      .finally(() => { if (active) setLoadingReview(false); });
    return () => { active = false; };
  }, [open, meetingId]);

  const handleApplyReview = async () => {
    if (!review || isApplying) return;
    setIsApplying(true);
    setError(null);
    try {
      const warning = await invoke<string | null>('apply_enhancement_review', { meetingId, reviewId: review.id });
      if (warning) toast.warning(warning); else toast.success('Proposed transcript applied.');
      setReview(null);
      onOpenChangeRef.current(false);
    } catch (error) { setError(String(error)); }
    finally { setIsApplying(false); }
  };

  const handleKeepSaved = async () => {
    if (!review || isApplying) return;
    setIsApplying(true);
    try {
      if (review.applied) {
        const warning=await invoke<string | null>('undo_enhancement', {meetingId,reviewId:review.id});
        if (warning) toast.warning(warning); else toast.success('Enhancement undone.');
      } else await invoke('discard_enhancement_review', { meetingId, reviewId: review.id });
      setReview(null);
      onOpenChangeRef.current(false);
    } catch (error) { setError(String(error)); }
    finally { setIsApplying(false); }
  };

  // Listen for retranscription events
  useEffect(() => {
    if (!open) return;

    const unlisteners: UnlistenFn[] = [];
    const cleanedUpRef = { current: false };

    // Progress is tracked by EnhancementRunProvider so it survives minimizing.
    const setupListeners = async () => {
      // Completion event
      const unlistenComplete = await listen<RetranscriptionResult>(
        'retranscription-complete',
        async (event) => {
          if (event.payload.meeting_id === meetingId) {
            try {
              const proposed = await invoke<EnhancementReview | null>('get_enhancement_review', { meetingId });
              if (event.payload.warning) toast.warning(event.payload.warning);
              else toast.success('Enhancement applied. Undo is available.');
              setReview(proposed);
              if (!proposed) setError('The proposed transcript could not be loaded. The saved transcript was kept.');
            } catch (error) { setError(String(error)); }
          }
        }
      );
      if (cleanedUpRef.current) {
        unlistenComplete();
        unlisteners.forEach(u => u());
        return;
      }
      unlisteners.push(unlistenComplete);

      // Error event
      const unlistenError = await listen<RetranscriptionError>(
        'retranscription-error',
        async (event) => {
          if (event.payload.meeting_id === meetingId) {
            setError(event.payload.error);
            setLoadingReview(true);
            try {
              const pending = await invoke<EnhancementReview | null>('get_enhancement_review', { meetingId });
              if (!cleanedUpRef.current) setReview(pending?.applied ? null : pending);
            } catch (error) {
              console.warn('Failed to load the pending enhancement:', error);
            } finally {
              if (!cleanedUpRef.current) setLoadingReview(false);
            }
          }
        }
      );
      if (cleanedUpRef.current) {
        unlistenError();
        unlisteners.forEach(u => u());
        return;
      }
      unlisteners.push(unlistenError);
    };

    setupListeners();

    return () => {
      cleanedUpRef.current = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [open, meetingId]);

  const handleStartRetranscription = async () => {
    if (!meetingFolderPath) {
      setError('Meeting folder path not available');
      return;
    }

    if (review || loadingReview || otherMeetingRunning || (speakerLabelsEnabled && checkingRefinement)) return;

    if (loadingModels || !selectedModelDetails) {
      setError(loadingModels ? 'Wait for the models to load.' : 'Select an available transcription model.');
      return;
    }

    markStarted(meetingId);
    setError(null);

    try {
      const languageToSend = isParakeetModel ? null : selectedLang === 'auto' ? null : selectedLang;
      await invoke('start_retranscription_command', {
        meetingId,
        meetingFolderPath,
        language: languageToSend,
        model: selectedModelDetails.name,
        provider: selectedModelDetails.provider,
        assignSpeakers: canAssignSpeakers && assignSpeakers,
      });
    } catch (err: any) {
      markStartFailed(meetingId);
      const errorMsg = typeof err === 'string' ? err : (err?.message || String(err));
      setError(errorMsg);
    }
  };

  const handleCancel = async () => {
    if (isProcessing && await cancelEnhancement()) {
      toast.info('Retranscription cancelled');
    }
    onOpenChange(false);
  };

  // Closing during processing minimizes the popup; the run keeps going.
  const handleOpenChange = (newOpen: boolean) => {
    if (!newOpen && isApplying) {
      return;
    }
    onOpenChange(newOpen);
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className={review ? "sm:max-w-5xl" : "sm:max-w-[450px]"}
        minimizable={isProcessing}
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {review ? (review.applied ? 'Enhancement applied' : 'Enhancement result') : isProcessing ? (
              <>
                <Loader2 className="h-5 w-5 animate-spin text-blue-600 dark:text-blue-400" />
                Retranscribing...
              </>
            ) : error ? (
              <>
                <AlertCircle className="h-5 w-5 text-red-600 dark:text-red-400" />
                Enhancement failed
              </>
            ) : (
              <>
                <RefreshCw className="h-5 w-5 text-blue-600 dark:text-blue-400" />
                Enhance meeting
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {review ? (review.applied ? 'Compare the changes or undo this enhancement.' : 'This result was not applied. Your saved transcript was kept.') : isProcessing
              ? progress?.message || 'Processing audio...'
              : error
                ? 'The enhancement could not finish'
                : 'Improve text and timing from the recording. The result is applied automatically, and Undo is available.'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {!isProcessing && !error && !review && (
            !isParakeetModel ? (
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  <Globe className="h-4 w-4 text-muted-foreground" />
                  <span className="text-sm font-medium">Language</span>
                </div>
                <Select value={selectedLang} onValueChange={setSelectedLang}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="Select language" />
                  </SelectTrigger>
                  <SelectContent className="max-h-60">
                    {languageOptions.map((lang) => (
                      <SelectItem key={lang.code} value={lang.code}>
                        {lang.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  {modelFindsSpeakers ? 'Auto sends no language hint. Select the meeting language when you know it.' : 'Select a specific language to improve accuracy, or use auto-detect'}
                </p>
              </div>
            ) : (
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  <Globe className="h-4 w-4 text-muted-foreground" />
                  <span className="text-sm font-medium">Language</span>
                </div>
                <p className="text-xs text-muted-foreground">
                  Language selection isn't supported for Parakeet. It always uses automatic detection.
                </p>
              </div>
            )
          )}

          {!isProcessing && !error && !review && otherMeetingRunning && (
            <p className="text-sm text-amber-700 dark:text-amber-300" role="status">Another meeting is being enhanced. Start this one when it finishes.</p>
          )}
          {!isProcessing && !error && !review && modelNotice && (
            <p className="text-sm text-amber-700 dark:text-amber-300" role="status">{modelNotice}</p>
          )}
          {!isProcessing && !error && !review && availableModels.length > 0 && (
            <div className="space-y-3">
              <div className="flex items-center gap-2">
                <Cpu className="h-4 w-4 text-muted-foreground" />
                <span className="text-sm font-medium">Model</span>
              </div>
              <Select value={selectedModelKey} onValueChange={setSelectedModelKey} disabled={loadingModels}>
                <SelectTrigger className="w-full">
                  <SelectValue placeholder={loadingModels ? "Loading models..." : "Select model"} />
                </SelectTrigger>
                <SelectContent>
                  {availableModels.map((model) => (
                    <SelectItem key={`${model.provider}:${model.name}`} value={`${model.provider}:${model.name}`}>
                      {model.displayName} ({Math.round(model.size_mb)} MB)
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                Choose a transcription model
              </p>
            </div>
          )}

          {!isProcessing && !error && !review && (
            <div className="flex items-center justify-between gap-4 rounded-md border px-3 py-2">
              <div>
                <label htmlFor="enhance-assign-speakers" className="text-sm font-medium">Assign speakers</label>
                <p className="text-xs text-muted-foreground">
                  {!speakerLabelsEnabled ? 'Enable speaker labels in Settings to use this.' : modelFindsSpeakers ? 'Nemotron identifies speakers while it transcribes.' : unavailableReason || 'Identify speakers in the new transcript.'}
                </p>
              </div>
              <Switch id="enhance-assign-speakers" checked={canAssignSpeakers && assignSpeakers}
                disabled={!canAssignSpeakers} onCheckedChange={setAssignSpeakers} />
            </div>
          )}

          {isProcessing && progress && (
            <div className="space-y-2">
              <div className="relative">
                <div className="w-full bg-gray-200 dark:bg-ink-800 rounded-full h-3">
                  <div
                    className="bg-blue-600 h-3 rounded-full transition-all duration-300 ease-out"
                    style={{ width: `${Math.min(progress.progress_percentage, 100)}%` }}
                  />
                </div>
                <div className="flex justify-between text-xs text-gray-600 dark:text-ink-300 mt-1">
                  <span>{progress.stage}</span>
                  <span>{Math.round(progress.progress_percentage)}%</span>
                </div>
              </div>
              <p className="text-sm text-muted-foreground text-center">
                {progress.message}
              </p>
            </div>
          )}

          {review && comparison && (
            <>
              {review.model && <p className="text-sm">Model: {review.model}</p>}
              {review.warnings.map(warning => <p key={warning} className="text-sm text-amber-700 dark:text-amber-300">{warning}</p>)}
              <div className="grid max-h-[55vh] grid-cols-1 gap-4 overflow-y-auto md:grid-cols-2">
                <section aria-label="Previous transcript" className="rounded-md border p-3">
                  <h3 className="mb-3 font-medium">Previous transcript</h3>
                  <p className="whitespace-pre-wrap text-sm">{comparison.before.map((part, i) => <span key={i} className={part.changed ? 'bg-red-100 text-red-900 dark:bg-red-900/40 dark:text-red-100' : undefined}>{part.text}</span>)}</p>
                </section>
                <section aria-label="Enhanced transcript" className="rounded-md border p-3">
                  <h3 className="mb-3 font-medium">Enhanced transcript</h3>
                  <p className="whitespace-pre-wrap text-sm">{comparison.after.map((part, i) => <span key={i} className={part.changed ? 'bg-green-100 text-green-900 dark:bg-green-900/40 dark:text-green-100' : undefined}>{part.text}</span>)}</p>
                </section>
              </div>
            </>
          )}

          {error && (
            <div className="bg-red-50 dark:bg-red-500/10 border border-red-200 dark:border-red-500/30 rounded-lg p-3">
              <p className="text-sm text-red-800 dark:text-red-200">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          {review && (
            <>
              <Button variant="outline" disabled={isApplying} onClick={handleKeepSaved}>{review.applied ? 'Undo enhancement' : 'Keep saved transcript'}</Button>
              {review.applied && <Button variant="outline" disabled={isApplying} onClick={() => {setReview(null);setError(null);}}>Enhance again</Button>}
              {review.applied
                ? <Button disabled={isApplying} onClick={() => onOpenChange(false)}>Done</Button>
                : <Button disabled={isApplying} onClick={handleApplyReview}>{isApplying ? 'Applying...' : 'Apply proposed transcript'}</Button>}
            </>
          )}
          {!isProcessing && !error && !review && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                onClick={handleStartRetranscription}
                className="bg-blue-600 hover:bg-blue-700"
                disabled={loadingReview || otherMeetingRunning || !meetingFolderPath || loadingModels || !selectedModelDetails || (speakerLabelsEnabled && checkingRefinement)}
              >
                <RefreshCw className="h-4 w-4 mr-2" />
                Start enhancement
              </Button>
            </>
          )}
          {isProcessing && (
            <Button variant="outline" onClick={handleCancel}>
              <X className="h-4 w-4 mr-2" />
              Cancel
            </Button>
          )}
          {error && !review && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Close
              </Button>
              <Button
                disabled={loadingReview}
                onClick={() => setError(null)}
                variant="outline"
              >
                Try Again
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** The one Enhance popup. It lives above the pages so a minimized run can be reopened from anywhere. */
export function EnhancementDialog() {
  const { dialog, setDialogOpen } = useEnhancementRun();
  if (!dialog) return null;
  return (
    <RetranscribeDialog
      key={dialog.meetingId}
      open={dialog.open}
      onOpenChange={setDialogOpen}
      meetingId={dialog.meetingId}
      meetingFolderPath={dialog.meetingFolderPath}
    />
  );
}
