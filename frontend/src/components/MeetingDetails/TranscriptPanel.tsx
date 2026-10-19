"use client";

import { MeetingSpeaker, Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { toast } from 'sonner';
import { useConfig } from '@/contexts/ConfigContext';

interface TranscriptPanelProps {
  transcripts: Transcript[];
  customPrompt: string;
  onPromptChange: (value: string) => void;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  isRecording: boolean;
  disableAutoScroll?: boolean;

  // Optional pagination props (when using virtualization)
  usePagination?: boolean;
  segments?: TranscriptSegmentData[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;

  // Retranscription props
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}

interface DiarizationStatus {
  meetingId: string;
  currentStatus: string;
  progress: number;
  method?: string | null;
  failureReason?: string | null;
  qualityFlags?: string | null;
}

export function TranscriptPanel({
  transcripts,
  customPrompt,
  onPromptChange,
  onCopyTranscript,
  onOpenMeetingFolder,
  isRecording,
  disableAutoScroll = false,
  usePagination = false,
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  // Convert transcripts to segments if pagination is not used but we want virtualization
  const convertedSegments = useMemo(() => {
    if (usePagination && segments) {
      return segments;
    }
    // Convert transcripts to segments for virtualization
    return transcripts.map(t => ({
      id: t.id,
      timestamp: t.audio_start_time ?? 0,
      endTime: t.audio_end_time,
      text: t.text,
      confidence: t.confidence,
      speaker_id: t.speaker_id,
      speaker_label: t.speaker_label,
      speaker_color: t.speaker_color,
      is_overlap: t.is_overlap,
      diarization_status: t.diarization_status,
      diarization_method: t.diarization_method,
      diarization_confidence: t.diarization_confidence,
    }));
  }, [transcripts, usePagination, segments]);

  const [speakers, setSpeakers] = useState<MeetingSpeaker[]>([]);
  const [isLoadingSpeakers, setIsLoadingSpeakers] = useState(false);
  const [editingSpeakerId, setEditingSpeakerId] = useState<string | null>(null);
  const [draftSpeakerLabel, setDraftSpeakerLabel] = useState('');
  const [savingSpeakerId, setSavingSpeakerId] = useState<string | null>(null);
  const [rememberingSpeakerId, setRememberingSpeakerId] = useState<string | null>(null);
  const [rememberedSpeakerIds, setRememberedSpeakerIds] = useState<Set<string>>(new Set());
  const [isApplyingCorrection, setIsApplyingCorrection] = useState(false);
  const [isMerging, setIsMerging] = useState(false);
  const [isStartingDiarization, setIsStartingDiarization] = useState(false);
  const mutationInFlight = useRef(false);
  const [mergingSpeakerId, setMergingSpeakerId] = useState<string | null>(null);
  const [mergeTargetId, setMergeTargetId] = useState('');
  const [correctionMode, setCorrectionMode] = useState(false);
  const [selectedRowIds, setSelectedRowIds] = useState<Set<string>>(new Set());
  const [assignmentTargetId, setAssignmentTargetId] = useState('');
  const [newSpeakerLabel, setNewSpeakerLabel] = useState('');
  const [assignmentOverlap, setAssignmentOverlap] = useState(false);
  const [diarizationStatus, setDiarizationStatus] = useState<DiarizationStatus | null>(null);
  const { diarizationSettings, isLoadingDiarizationSettings } = useConfig();
  const diarizationEnabled = !isLoadingDiarizationSettings
    && diarizationSettings.enabled && diarizationSettings.mode !== 'off';

  const speakerSourceCount = usePagination
    ? totalCount ?? convertedSegments.length
    : transcripts.length;

  const refinementActive = isStartingDiarization || [
    'processing', 'preparing_models', 'rebuilding_windows', 'transcribing_windows', 'saving',
  ].includes(diarizationStatus?.currentStatus || '');
  const recoveryRequired = diarizationStatus?.currentStatus === 'recovery_required';
  const editsDisabled = refinementActive || recoveryRequired || savingSpeakerId !== null
    || rememberingSpeakerId !== null || isApplyingCorrection || isMerging;
  const beginEdit = () => {
    if (editsDisabled || mutationInFlight.current) return false;
    mutationInFlight.current = true;
    return true;
  };

  useEffect(() => {
    setEditingSpeakerId(null);
    setMergingSpeakerId(null);
    setCorrectionMode(false);
    setSelectedRowIds(new Set());
    setRememberedSpeakerIds(new Set());
    setAssignmentTargetId('');
    setAssignmentOverlap(false);
    setNewSpeakerLabel('');
    setDiarizationStatus(null);
  }, [meetingId]);

  const loadSpeakers = useCallback(async () => {
    if (!meetingId || speakerSourceCount === 0) {
      setSpeakers([]);
      return;
    }

    setIsLoadingSpeakers(true);
    try {
      const loadedSpeakers = await invoke<MeetingSpeaker[]>('get_meeting_speakers', { meetingId });
      setSpeakers(loadedSpeakers);
    } catch (error) {
      console.error('Failed to load meeting speakers:', error);
      toast.error('Failed to load speakers');
    } finally {
      setIsLoadingSpeakers(false);
    }
  }, [meetingId, speakerSourceCount]);

  useEffect(() => {
    if (isRecording) {
      setSpeakers([]);
      setEditingSpeakerId(null);
      return;
    }

    void loadSpeakers();
  }, [isRecording, loadSpeakers]);

  useEffect(() => {
    if (!meetingId || isRecording) {
      setDiarizationStatus(null);
      return;
    }
    void invoke<DiarizationStatus | null>('get_meeting_diarization_status', { meetingId })
      .then(setDiarizationStatus)
      .catch(error => console.warn('Failed to load speaker refinement status:', error));

    let unlisten: (() => void) | undefined;
    let disposed = false;
    void listen<{ meetingId: string; status: string; progress: number; error?: string }>(
      'diarization-progress',
      event => {
        if (event.payload.meetingId !== meetingId) return;
        setDiarizationStatus(previous => ({
          meetingId,
          currentStatus: event.payload.status,
          progress: event.payload.progress,
          failureReason: event.payload.error || null,
          method: previous?.method,
          qualityFlags: previous?.qualityFlags,
        }));
        if (event.payload.status === 'completed') {
          void onRefetchTranscripts?.();
          void loadSpeakers();
        }
      }
    ).then(cleanup => { if (disposed) cleanup(); else unlisten = cleanup; });
    return () => { disposed = true; unlisten?.(); };
  }, [isRecording, loadSpeakers, meetingId, onRefetchTranscripts]);

  const runDiarization = async (retry: boolean) => {
    if (!meetingId || !diarizationEnabled || !beginEdit()) return;
    setIsStartingDiarization(true);
    const command = retry ? 'retry_final_diarization' : 'start_final_diarization';
    try {
      try {
        await invoke(command, { request: { meetingId, overwriteManualEdits: false } });
      } catch (error) {
        if (!String(error).includes('manual speaker edits')) throw error;
        const overwrite = await confirm(
          'This run will replace your manual speaker corrections. Continue?',
          { title: 'Replace manual speaker edits?', kind: 'warning' }
        );
        if (!overwrite) return;
        await invoke(command, { request: { meetingId, overwriteManualEdits: true } });
      }
      await onRefetchTranscripts?.();
      await loadSpeakers();
    } catch (error) {
      toast.error('Speaker processing could not finish', { description: String(error) });
    } finally {
      setIsStartingDiarization(false);
      mutationInFlight.current = false;
    }
  };

  const mergeSpeaker = async () => {
    if (!meetingId || !mergingSpeakerId || !mergeTargetId || editsDisabled) return;
    const target = speakers.find(speaker => speaker.speakerId === mergeTargetId);
    if (!target || !beginEdit()) return;
    setIsMerging(true);
    try {
      await invoke('merge_speakers', {
        request: {
          meetingId,
          sourceSpeakerIds: [mergingSpeakerId],
          targetSpeakerId: target.speakerId,
          targetLabel: target.speakerLabel,
        }
      });
      setMergingSpeakerId(null);
      setMergeTargetId('');
      await onRefetchTranscripts?.();
      await loadSpeakers();
      toast.success('Speakers merged');
    } catch (error) {
      toast.error('Could not merge speakers', { description: String(error) });
    } finally {
      setIsMerging(false);
      mutationInFlight.current = false;
    }
  };

  const toggleSelectedRow = (id: string) => {
    if (editsDisabled) return;
    setSelectedRowIds(current => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  };

  const applyRowCorrection = async () => {
    if (!meetingId || selectedRowIds.size === 0 || editsDisabled) return;
    const target = speakers.find(speaker => speaker.speakerId === assignmentTargetId);
    const creating = assignmentTargetId === '__new';
    const label = creating ? newSpeakerLabel.trim() : target?.speakerLabel;
    if (!assignmentOverlap && (!assignmentTargetId || !label)) {
      toast.error('Choose a speaker or enter a new speaker name');
      return;
    }
    const speakerId = assignmentOverlap
      ? null
      : creating
        ? `manual-${Date.now()}`
        : target?.speakerId;
    if (!beginEdit()) return;
    setIsApplyingCorrection(true);
    try {
      await invoke('reassign_transcripts', {
        request: {
          meetingId,
          transcriptIds: Array.from(selectedRowIds),
          speakerId,
          speakerLabel: assignmentOverlap ? null : label,
          isOverlap: assignmentOverlap,
          candidateSpeakerIds: assignmentOverlap && target ? [target.speakerId] : [],
        }
      });
      setSelectedRowIds(new Set());
      setCorrectionMode(false);
      setAssignmentTargetId('');
      setNewSpeakerLabel('');
      setAssignmentOverlap(false);
      await onRefetchTranscripts?.();
      await loadSpeakers();
      toast.success('Transcript rows updated');
    } catch (error) {
      toast.error('Could not update transcript rows', { description: String(error) });
    } finally {
      setIsApplyingCorrection(false);
      mutationInFlight.current = false;
    }
  };

  const startEditingSpeaker = (speaker: MeetingSpeaker) => {
    if (editsDisabled) return;
    setEditingSpeakerId(speaker.speakerId);
    setDraftSpeakerLabel(speaker.speakerLabel);
  };

  const cancelEditingSpeaker = useCallback(() => {
    setEditingSpeakerId(null);
    setDraftSpeakerLabel('');
  }, []);

  const saveSpeakerLabel = async (speaker: MeetingSpeaker) => {
    if (!meetingId || editsDisabled) {
      return;
    }

    const label = draftSpeakerLabel.trim();
    if (!label) {
      toast.error('Speaker name cannot be empty');
      return;
    }

    if (label === speaker.speakerLabel) {
      cancelEditingSpeaker();
      return;
    }

    if (!beginEdit()) return;
    setSavingSpeakerId(speaker.speakerId);
    try {
      await invoke('rename_speaker', {
        request: {
          meetingId,
          speakerId: speaker.speakerId,
          label,
        },
      });
      setSpeakers(previousSpeakers =>
        previousSpeakers.map(previousSpeaker =>
          previousSpeaker.speakerId === speaker.speakerId
            ? { ...previousSpeaker, speakerLabel: label }
            : previousSpeaker
        )
      );
      cancelEditingSpeaker();
      setRememberedSpeakerIds(current => {
        const next = new Set(current);
        next.delete(speaker.speakerId);
        return next;
      });

      if (onRefetchTranscripts) {
        await onRefetchTranscripts();
      }
      await loadSpeakers();

      toast.success('Speaker renamed');
    } catch (error) {
      console.error('Failed to rename speaker:', error);
      toast.error('Failed to rename speaker', {
        description: String(error),
      });
    } finally {
      setSavingSpeakerId(null);
      mutationInFlight.current = false;
    }
  };

  const rememberSpeaker = async (speaker: MeetingSpeaker) => {
    if (!meetingId || !beginEdit()) return;
    setRememberingSpeakerId(speaker.speakerId);
    try {
      await invoke('remember_speaker', {
        request: { meetingId, speakerId: speaker.speakerId, name: speaker.speakerLabel },
      });
      setRememberedSpeakerIds(current => new Set(current).add(speaker.speakerId));
      toast.success(`Voice remembered for ${speaker.speakerLabel}`);
    } catch (error) {
      toast.error('Could not remember voice', { description: String(error) });
    } finally {
      setRememberingSpeakerId(null);
      mutationInFlight.current = false;
    }
  };

  return (
    <div className="flex h-full min-w-0 w-full bg-white flex-col relative @container">
      {/* Title area */}
      <div className="p-4 border-b border-gray-200">
        <TranscriptButtonGroup
          transcriptCount={usePagination ? (totalCount ?? convertedSegments.length) : (transcripts?.length || 0)}
          onCopyTranscript={onCopyTranscript}
          onOpenMeetingFolder={onOpenMeetingFolder}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onRefetchTranscripts={onRefetchTranscripts}
        />
      </div>

      {!isRecording && diarizationStatus && !['none', 'completed'].includes(diarizationStatus.currentStatus) && (
        <div className={`border-b px-4 py-2 text-sm ${
          ['fallback_to_live', 'needs_review', 'failed', 'cancelled', 'recovery_required'].includes(diarizationStatus.currentStatus)
            ? 'border-amber-200 bg-amber-50 text-amber-950'
            : 'border-blue-200 bg-blue-50 text-blue-950'
        }`}>
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="min-w-0">
              <p className="font-medium">
                {['fallback_to_live', 'needs_review', 'failed', 'cancelled', 'recovery_required'].includes(diarizationStatus.currentStatus)
                    ? diarizationStatus.currentStatus === 'recovery_required'
                      ? 'Transcript recovery is required'
                      : 'Speaker processing needs attention'
                    : 'Refining speakers locally…'}
              </p>
              <p className="break-words text-xs opacity-80">
                {diarizationStatus.failureReason || `${diarizationStatus.progress}% complete`}
              </p>
            </div>
            {refinementActive ? (
              <Button size="sm" variant="ghost" onClick={() => void invoke('cancel_final_diarization')}>
                Cancel
              </Button>
            ) : ['fallback_to_live', 'needs_review', 'failed', 'cancelled'].includes(diarizationStatus.currentStatus) ? (
              <Button size="sm" variant="outline" disabled={editsDisabled || !diarizationEnabled} onClick={() => void runDiarization(true)}>
                Retry
              </Button>
            ) : null}
          </div>
          {recoveryRequired && (
            <p className="mt-2 text-xs">Close and reopen the app to retry transcript recovery. Speaker changes are unavailable until recovery finishes.</p>
          )}
        </div>
      )}

      {!isRecording && meetingId && (speakerSourceCount > 0 || isLoadingSpeakers || (meetingFolderPath && diarizationEnabled)) && (
        <div className="shrink-0 px-4 py-2 border-b border-border space-y-1.5">
          <div className="flex flex-wrap items-center justify-between gap-x-2 gap-y-1">
            <div>
              <p className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                Speakers{speakers.length > 0 ? ` (${speakers.length})` : ''}
              </p>
            </div>
            <div className="flex items-center gap-1">
              {isLoadingSpeakers && (
                <span className="text-xs text-muted-foreground">Loading…</span>
              )}
              {meetingFolderPath && diarizationEnabled && (!diarizationStatus || ['none', 'completed'].includes(diarizationStatus.currentStatus)) && (
                <Button size="sm" variant="ghost" className="h-7 px-2"
                  title="Analyze this meeting locally and update speaker labels. Transcript text stays the same."
                  disabled={editsDisabled}
                  onClick={() => void runDiarization(diarizationStatus?.currentStatus === 'completed')}>
                  {isStartingDiarization ? 'Starting…' : diarizationStatus?.currentStatus === 'completed' ? 'Run again' : 'Identify speakers'}
                </Button>
              )}
              {!isLoadingSpeakers && speakerSourceCount > 0 && (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2"
                  disabled={editsDisabled}
                  onClick={() => {
                    if (editsDisabled) return;
                    setCorrectionMode(value => !value);
                    setSelectedRowIds(new Set());
                  }}
                >
                  {correctionMode ? 'Done' : 'Correct rows'}
                </Button>
              )}
            </div>
          </div>

          {refinementActive && (
            <p className="text-xs text-muted-foreground">Wait for speaker processing to finish before you change speakers.</p>
          )}
          {!isLoadingSpeakers && speakers.length === 0 && (
            <p className="text-xs text-muted-foreground">No named speakers yet. Use Correct rows to assign a speaker.</p>
          )}
          <div className="max-h-40 divide-y divide-border overflow-y-auto">
            {speakers.map((speaker) => {
              const isEditing = editingSpeakerId === speaker.speakerId;

              return (
                <div
                  key={speaker.speakerId}
                  className="py-1"
                >
                  <div className="flex items-center gap-1">
                    <span
                      className="h-2 w-2 rounded-full border border-border shrink-0"
                      style={{ backgroundColor: speaker.speakerColor || '#94a3b8' }}
                    />

                    {isEditing ? (
                      <>
                        <Input
                          aria-label="Speaker name"
                          className="h-8 min-w-[120px] flex-1"
                          value={draftSpeakerLabel}
                          disabled={editsDisabled}
                          onChange={(event) => setDraftSpeakerLabel(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === 'Enter') {
                              void saveSpeakerLabel(speaker);
                            }
                            if (event.key === 'Escape') {
                              cancelEditingSpeaker();
                            }
                          }}
                        />
                        <Button
                          size="sm"
                          onClick={() => void saveSpeakerLabel(speaker)}
                          disabled={editsDisabled}
                        >
                          Save
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={cancelEditingSpeaker}
                          disabled={editsDisabled}
                        >
                          Cancel
                        </Button>
                      </>
                    ) : (
                      <>
                        <div className="flex min-w-0 flex-1 items-baseline gap-1.5" title={`${speaker.speakerLabel} · ${speaker.segmentCount} transcript segments`}>
                          <span className="truncate text-sm font-medium text-foreground">
                            {speaker.speakerLabel}
                          </span>
                          <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground" aria-label={`${speaker.segmentCount} transcript segments`}>
                            {speaker.segmentCount}
                          </span>
                        </div>
                        <Button
                          size="sm"
                          className="h-7 shrink-0 px-1.5 text-[11px]"
                          variant="ghost"
                          onClick={() => startEditingSpeaker(speaker)}
                          disabled={editsDisabled}
                        >
                          Rename
                        </Button>
                        {speakers.length > 1 && mergingSpeakerId !== speaker.speakerId && (
                          <Button
                            size="sm"
                            className="h-7 shrink-0 px-1.5 text-[11px]"
                            variant="ghost"
                            disabled={editsDisabled}
                            onClick={() => {
                              if (editsDisabled) return;
                              setMergingSpeakerId(speaker.speakerId);
                              setMergeTargetId('');
                            }}
                          >
                            Merge
                          </Button>
                        )}
                        <Button size="sm" variant="ghost"
                          className="h-7 shrink-0 px-1.5 text-[11px]"
                          title="Save this voice and name for future meetings."
                          disabled={editsDisabled || rememberedSpeakerIds.has(speaker.speakerId)}
                          onClick={() => void rememberSpeaker(speaker)}>
                          {rememberingSpeakerId === speaker.speakerId ? 'Remembering…'
                            : rememberedSpeakerIds.has(speaker.speakerId) ? 'Voice remembered' : 'Remember voice'}
                        </Button>
                      </>
                    )}
                  </div>
                  {mergingSpeakerId === speaker.speakerId && (
                    <div className="mt-2 flex flex-wrap items-center gap-2">
                      <select
                        aria-label="Merge into speaker"
                        className="h-8 min-w-0 flex-1 rounded-md border border-border bg-background px-2 text-xs"
                        disabled={editsDisabled}
                        value={mergeTargetId}
                        onChange={event => setMergeTargetId(event.target.value)}
                      >
                        <option value="">Merge into…</option>
                        {speakers.filter(target => target.speakerId !== speaker.speakerId).map(target => (
                          <option key={target.speakerId} value={target.speakerId}>{target.speakerLabel}</option>
                        ))}
                      </select>
                      <Button size="sm" disabled={!mergeTargetId || editsDisabled} onClick={() => void mergeSpeaker()}>Merge</Button>
                      <Button size="sm" variant="ghost" disabled={editsDisabled} onClick={() => setMergingSpeakerId(null)}>Cancel</Button>
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          {correctionMode && (
            <div className="space-y-2 rounded-md border border-border bg-background p-2">
              <p className="text-xs text-muted-foreground">
                Select transcript rows below, then assign a speaker or mark overlap.
              </p>
              <div className="flex flex-wrap items-center gap-2">
                <select
                  className="h-8 min-w-0 flex-1 rounded-md border border-border bg-background px-2 text-xs"
                  aria-label="Assign selected rows to speaker"
                  value={assignmentTargetId}
                  disabled={assignmentOverlap || editsDisabled}
                  onChange={event => setAssignmentTargetId(event.target.value)}
                >
                  <option value="">Assign to…</option>
                  {speakers.map(speaker => (
                    <option key={speaker.speakerId} value={speaker.speakerId}>{speaker.speakerLabel}</option>
                  ))}
                  <option value="__new">New speaker…</option>
                </select>
                {assignmentTargetId === '__new' && !assignmentOverlap && (
                  <Input
                    className="h-8"
                    disabled={editsDisabled}
                    placeholder="New speaker name"
                    value={newSpeakerLabel}
                    onChange={event => setNewSpeakerLabel(event.target.value)}
                  />
                )}
                <label className="flex items-center gap-1 text-xs">
                  <input
                    type="checkbox"
                    disabled={editsDisabled}
                    checked={assignmentOverlap}
                    onChange={event => setAssignmentOverlap(event.target.checked)}
                  />
                  Multiple speakers
                </label>
                <Button size="sm" disabled={selectedRowIds.size === 0 || editsDisabled} onClick={() => void applyRowCorrection()}>
                  Apply ({selectedRowIds.size})
                </Button>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Transcript content - use virtualized view for better performance */}
      <div className="flex-1 overflow-hidden pb-4">
        <VirtualizedTranscriptView
          segments={convertedSegments}
          isRecording={isRecording}
          isPaused={false}
          isProcessing={false}
          isStopping={false}
          enableStreaming={false}
          showConfidence={true}
          disableAutoScroll={disableAutoScroll}
          hasMore={hasMore}
          isLoadingMore={isLoadingMore}
          totalCount={totalCount}
          loadedCount={loadedCount}
          onLoadMore={onLoadMore}
          selectedSegmentIds={correctionMode && !editsDisabled ? selectedRowIds : undefined}
          onToggleSegment={correctionMode && !editsDisabled ? toggleSelectedRow : undefined}
        />
      </div>

      {/* Custom prompt input at bottom of transcript section */}
      {!isRecording && convertedSegments.length > 0 && (
        <div className="p-1 border-t border-gray-200">
          <textarea
            placeholder="Add context for AI summary. For example people involved, meeting overview, objective etc..."
            className="w-full px-3 py-2 border border-gray-200 rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 bg-white shadow-sm min-h-[80px] resize-y"
            value={customPrompt}
            onChange={(e) => onPromptChange(e.target.value)}
          />
        </div>
      )}
    </div>
  );
}
