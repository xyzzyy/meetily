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
import { Input } from '../ui/input';
import { useTranscriptEnhancement } from '@/contexts/TranscriptEnhancementContext';
import { useConfig } from '@/contexts/ConfigContext';
import { useSpeakerRefinementAvailability } from '@/hooks/useSpeakerRefinementAvailability';
import { LANGUAGES } from '@/constants/languages';
import { languagesForProvider } from '@/lib/funasr';
import { useTranscriptionModels, ModelOption } from '@/hooks/useTranscriptionModels';

interface RetranscribeDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  meetingFolderPath: string | null;
  trim?: boolean;
}

export function RetranscribeDialog({
  open,
  onOpenChange,
  meetingId,
  meetingFolderPath,
  trim = false,
}: RetranscribeDialogProps) {
  const { selectedLanguage, transcriptModelConfig, diarizationSettings, isLoadingDiarizationSettings } = useConfig();
  const { supported: refinementSupported, checking: checkingRefinement, unavailableReason } = useSpeakerRefinementAvailability();
  const speakerLabelsEnabled = !isLoadingDiarizationSettings && diarizationSettings.enabled;
  const canAssignSpeakers = speakerLabelsEnabled && refinementSupported;
  const [assignSpeakers, setAssignSpeakers] = useState(canAssignSpeakers);
  const enhancement = useTranscriptEnhancement();
  const isProcessing = enhancement.job?.request.meetingId === meetingId;
  const progress = isProcessing ? enhancement.job?.progress : null;
  const [error, setError] = useState<string | null>(null);
  const [selectedLang, setSelectedLang] = useState(selectedLanguage || 'auto');
  const [duration, setDuration] = useState<number | null>(null);
  const [startSeconds, setStartSeconds] = useState('0');
  const [endSeconds, setEndSeconds] = useState('');
  const [trimInfoRequest, setTrimInfoRequest] = useState(0);
  const startTime = Number(startSeconds);
  const endTime = Number(endSeconds);
  const validRange = duration !== null && startSeconds.trim() !== '' && endSeconds.trim() !== ''
    && Number.isFinite(startTime) && Number.isFinite(endTime) && startTime >= 0
    && endTime > startTime && endTime <= duration && (startTime > 0 || endTime < duration);


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
      setError(null);
      setSelectedLang(selectedLanguage || 'auto');

      // Fetch available models using centralized hook
      fetchModels();
    }
  }, [open, selectedLanguage, transcriptModelConfig, fetchModels, resetSelection]);

  useEffect(() => {
    if (!open || !trim || isProcessing) return;
    let cancelled = false;
    setDuration(null);
    setStartSeconds('0');
    setEndSeconds('');
    invoke<number>('get_recording_trim_info', { meetingId }).then(value => {
      if (cancelled) return;
      setDuration(value);
      setEndSeconds(String(value));
    }).catch(error => { if (!cancelled) setError(String(error)); });
    return () => { cancelled = true; };
  }, [open, trim, meetingId, trimInfoRequest, isProcessing]);

  // Default to the app-wide speaker labels setting, including when it finishes
  // loading after the dialog is already open.
  useEffect(() => {
    if (open) setAssignSpeakers(canAssignSpeakers);
  }, [open, canAssignSpeakers]);

  const handleStartRetranscription = async () => {
    if (!meetingFolderPath) {
      setError('Meeting folder path not available');
      return;
    }

    if (speakerLabelsEnabled && checkingRefinement) return;

    if (loadingModels || !selectedModelDetails) {
      setError(loadingModels ? 'Wait for the models to load.' : 'Select an available transcription model.');
      return;
    }

    if (trim && !validRange) {
      setError('Set a start and end time within the recording. Select a shorter range to keep.');
      return;
    }
    setError(null);

    try {
      const languageToSend = isParakeetModel ? null : selectedLang === 'auto' ? null : selectedLang;
      enhancement.start({
        meetingId,
        meetingFolderPath,
        language: languageToSend,
        model: selectedModelDetails.name,
        provider: selectedModelDetails.provider,
        assignSpeakers: canAssignSpeakers && assignSpeakers,
        ...(trim ? { range: { start_seconds: startTime, end_seconds: endTime } } : {}),
      });
      onOpenChange(false);
    } catch (err: any) {
      const errorMsg = typeof err === 'string' ? err : (err?.message || String(err));
      setError(errorMsg);

    }
  };

  const handleCancel = async () => {
    await enhancement.cancel();
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[450px] max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {isProcessing ? (
              <>
                <Loader2 className="h-5 w-5 animate-spin text-blue-600 dark:text-blue-400" />
                Retranscribing...
              </>
            ) : error ? (
              <>
                <AlertCircle className="h-5 w-5 text-red-600 dark:text-red-400" />
                {trim ? 'Trim could not start' : 'Enhancement failed'}
              </>
            ) : (
              <>
                <RefreshCw className="h-5 w-5 text-blue-600 dark:text-blue-400" />
                {trim ? 'Trim recording' : 'Enhance meeting'}
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {isProcessing
              ? progress?.message || 'Processing audio...'
              : error
                ? 'The enhancement could not finish'
                : trim
                  ? 'Keep the selected range. This deletes the original recording, old speaker audio, transcript, and summary. Then the app creates a new transcript. This cannot be undone.'
                  : 'Create new text and timing from the recording. The saved transcript stays unchanged until enhancement succeeds.'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {trim && !isProcessing && !error && (
            <fieldset className="space-y-2" disabled={duration === null}>
              <legend className="text-sm font-medium">Audio to keep</legend>
              {duration === null ? <p role="status">Loading recording duration...</p> : (
                <>
                  <p className="text-xs text-muted-foreground">Recording length: {duration.toFixed(3)} seconds. Times start at zero.</p>
                  <div className="grid grid-cols-2 gap-3">
                    <div><label htmlFor="trim-start" className="text-sm">Start (seconds)</label>
                      <Input id="trim-start" type="number" min="0" max={duration} step="0.001" value={startSeconds} onChange={event => setStartSeconds(event.target.value)} /></div>
                    <div><label htmlFor="trim-end" className="text-sm">End (seconds)</label>
                      <Input id="trim-end" type="number" min="0" max={duration} step="0.001" value={endSeconds} onChange={event => setEndSeconds(event.target.value)} /></div>
                  </div>
                  <p className="text-xs text-muted-foreground" role="status">{validRange ? `Keep ${(endTime - startTime).toFixed(3)} seconds.` : 'Select a shorter range. The end must be after the start.'}</p>
                </>
              )}
            </fieldset>
          )}

          {!isProcessing && !error && (
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
                  Select a specific language to improve accuracy, or use auto-detect
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

          {enhancement.job && !isProcessing && (
            <p role="status" className="text-sm text-muted-foreground">
              Another meeting is being enhanced. You can continue to use the app.
            </p>
          )}
          {!isProcessing && !error && modelNotice && (
            <p className="text-sm text-amber-700 dark:text-amber-300" role="status">{modelNotice}</p>
          )}
          {!isProcessing && !error && availableModels.length > 0 && (
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

          {!isProcessing && !error && (
            <div className="flex items-center justify-between gap-4 rounded-md border px-3 py-2">
              <div>
                <label htmlFor="enhance-assign-speakers" className="text-sm font-medium">Assign speakers</label>
                <p className="text-xs text-muted-foreground">
                  {!speakerLabelsEnabled ? 'Enable speaker labels in Settings to use this.' : unavailableReason || 'Identify speakers in the new transcript.'}
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

          {error && (
            <div className="bg-red-50 dark:bg-red-500/10 border border-red-200 dark:border-red-500/30 rounded-lg p-3">
              <p className="text-sm text-red-800 dark:text-red-200">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          {!isProcessing && !error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                onClick={handleStartRetranscription}
                className="bg-blue-600 hover:bg-blue-700"
                disabled={!!enhancement.job || !meetingFolderPath || loadingModels || !selectedModelDetails || (speakerLabelsEnabled && checkingRefinement) || (trim && !validRange)}
              >
                <RefreshCw className="h-4 w-4 mr-2" />
                {trim ? 'Trim and transcribe' : 'Start enhancement'}
              </Button>
            </>
          )}
          {isProcessing && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>Run in background</Button>
              <Button variant="outline" disabled={enhancement.job?.cancelling} onClick={handleCancel}>
                <X className="h-4 w-4 mr-2" />
                {enhancement.job?.cancelling ? 'Cancelling...' : 'Cancel enhancement'}
              </Button>
            </>
          )}
          {error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Close
              </Button>
              <Button
                onClick={() => {
                  setError(null);
                  if (trim && duration === null) setTrimInfoRequest(value => value + 1);
                }}
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
