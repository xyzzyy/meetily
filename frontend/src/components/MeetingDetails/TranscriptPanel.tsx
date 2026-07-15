"use client";

import { MeetingSpeaker, Transcript, TranscriptSegmentData } from '@/types';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useMemo, useState } from 'react';
import { toast } from 'sonner';

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

interface DiarizationSettings {
  enabled: boolean;
  postCallRefinementEnabled: boolean;
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
  const [rememberVoice, setRememberVoice] = useState(false);
  const [mergingSpeakerId, setMergingSpeakerId] = useState<string | null>(null);
  const [mergeTargetId, setMergeTargetId] = useState('');
  const [correctionMode, setCorrectionMode] = useState(false);
  const [selectedRowIds, setSelectedRowIds] = useState<Set<string>>(new Set());
  const [assignmentTargetId, setAssignmentTargetId] = useState('');
  const [newSpeakerLabel, setNewSpeakerLabel] = useState('');
  const [assignmentOverlap, setAssignmentOverlap] = useState(false);
  const [diarizationStatus, setDiarizationStatus] = useState<DiarizationStatus | null>(null);
  const [diarizationEnabled, setDiarizationEnabled] = useState(false);

  const speakerSourceCount = usePagination
    ? totalCount ?? convertedSegments.length
    : transcripts.length;

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
    ).then(cleanup => { unlisten = cleanup; });
    return () => unlisten?.();
  }, [isRecording, loadSpeakers, meetingId, onRefetchTranscripts]);

  useEffect(() => {
    if (isRecording) return;
    void invoke<DiarizationSettings>('get_diarization_settings')
      .then(settings => setDiarizationEnabled(
        settings.enabled && settings.postCallRefinementEnabled
      ))
      .catch(() => setDiarizationEnabled(false));
  }, [isRecording]);

  const startDiarization = useCallback(async () => {
    if (!meetingId) return;
    try {
      await invoke('start_final_diarization', {
        request: { meetingId, overwriteManualEdits: false }
      });
    } catch (error) {
      toast.error('Speaker recognition could not finish', { description: String(error) });
    }
  }, [meetingId]);

  const retryDiarization = useCallback(async () => {
    if (!meetingId) return;
    try {
      await invoke('retry_final_diarization', {
        request: { meetingId, overwriteManualEdits: false }
      });
    } catch (error) {
      if (String(error).includes('manual speaker edits')) {
        const overwrite = await confirm(
          'This rerun will replace your manual speaker corrections. Continue?',
          { title: 'Replace manual speaker edits?', kind: 'warning' }
        );
        if (overwrite) {
          await invoke('retry_final_diarization', {
            request: { meetingId, overwriteManualEdits: true }
          });
          return;
        }
      }
      toast.error('Speaker refinement could not finish', { description: String(error) });
    }
  }, [meetingId]);

  const mergeSpeaker = useCallback(async () => {
    if (!meetingId || !mergingSpeakerId || !mergeTargetId) return;
    const target = speakers.find(speaker => speaker.speakerId === mergeTargetId);
    if (!target) return;
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
    }
  }, [loadSpeakers, meetingId, mergeTargetId, mergingSpeakerId, onRefetchTranscripts, speakers]);

  const toggleSelectedRow = useCallback((id: string) => {
    setSelectedRowIds(current => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  }, []);

  const applyRowCorrection = useCallback(async () => {
    if (!meetingId || selectedRowIds.size === 0) return;
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
      await onRefetchTranscripts?.();
      await loadSpeakers();
      toast.success('Transcript rows updated');
    } catch (error) {
      toast.error('Could not update transcript rows', { description: String(error) });
    }
  }, [assignmentOverlap, assignmentTargetId, loadSpeakers, meetingId, newSpeakerLabel, onRefetchTranscripts, selectedRowIds, speakers]);

  const startEditingSpeaker = useCallback((speaker: MeetingSpeaker) => {
    setEditingSpeakerId(speaker.speakerId);
    setDraftSpeakerLabel(speaker.speakerLabel);
    setRememberVoice(false);
  }, []);

  const cancelEditingSpeaker = useCallback(() => {
    setEditingSpeakerId(null);
    setDraftSpeakerLabel('');
  }, []);

  const saveSpeakerLabel = useCallback(async (speaker: MeetingSpeaker) => {
    if (!meetingId) {
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

    setSavingSpeakerId(speaker.speakerId);
    try {
      await invoke('rename_speaker', {
        request: {
          meetingId,
          speakerId: speaker.speakerId,
          label,
        },
      });
      if (rememberVoice) {
        await invoke('remember_speaker', {
          request: { meetingId, speakerId: speaker.speakerId, name: label }
        });
      }

      setSpeakers(previousSpeakers =>
        previousSpeakers.map(previousSpeaker =>
          previousSpeaker.speakerId === speaker.speakerId
            ? { ...previousSpeaker, speakerLabel: label }
            : previousSpeaker
        )
      );
      cancelEditingSpeaker();

      if (onRefetchTranscripts) {
        await onRefetchTranscripts();
      }
      await loadSpeakers();

      toast.success(rememberVoice ? 'Speaker renamed and voice remembered' : 'Speaker renamed');
    } catch (error) {
      console.error('Failed to rename speaker:', error);
      toast.error('Failed to rename speaker', {
        description: String(error),
      });
    } finally {
      setSavingSpeakerId(null);
    }
  }, [cancelEditingSpeaker, draftSpeakerLabel, loadSpeakers, meetingId, onRefetchTranscripts, rememberVoice]);

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

      {!isRecording && diarizationStatus && diarizationStatus.currentStatus !== 'none' && (
        <div className={`border-b px-4 py-3 text-sm ${
          ['fallback_to_live', 'needs_review', 'failed', 'cancelled'].includes(diarizationStatus.currentStatus)
            ? 'border-amber-200 bg-amber-50 text-amber-950'
            : 'border-blue-200 bg-blue-50 text-blue-950'
        }`}>
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <p className="font-medium">
                {diarizationStatus.currentStatus === 'completed'
                  ? 'Speaker recognition complete'
                  : ['fallback_to_live', 'needs_review', 'failed', 'cancelled'].includes(diarizationStatus.currentStatus)
                    ? 'Using the live transcript'
                    : 'Refining speakers locally…'}
              </p>
              <p className="truncate text-xs opacity-80">
                {diarizationStatus.failureReason || `${diarizationStatus.progress}% complete`}
              </p>
            </div>
            {['processing', 'preparing_models', 'rebuilding_windows', 'transcribing_windows', 'saving'].includes(diarizationStatus.currentStatus) ? (
              <Button size="sm" variant="ghost" onClick={() => void invoke('cancel_final_diarization')}>
                Cancel
              </Button>
            ) : ['fallback_to_live', 'needs_review', 'failed', 'cancelled'].includes(diarizationStatus.currentStatus) ? (
              <Button size="sm" variant="outline" onClick={() => void retryDiarization()}>
                Retry
              </Button>
            ) : null}
          </div>
        </div>
      )}

      {!isRecording && meetingId && meetingFolderPath && diarizationEnabled && !diarizationStatus && (
        <div className="flex items-center justify-between gap-3 border-b border-blue-200 bg-blue-50 px-4 py-3 text-sm text-blue-950">
          <div>
            <p className="font-medium">Speaker recognition is available</p>
            <p className="text-xs opacity-80">Process this meeting locally and rebuild the transcript by speaker.</p>
          </div>
          <Button size="sm" variant="outline" onClick={() => void startDiarization()}>
            Run
          </Button>
        </div>
      )}

      {!isRecording && meetingId && (speakers.length > 0 || isLoadingSpeakers) && (
        <div className="px-4 py-3 border-b border-border space-y-3">
          <div className="flex items-start justify-between gap-3">
            <div>
              <p className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                Speakers{speakers.length > 0 ? ` (${speakers.length})` : ''}
              </p>
              <p className="text-xs text-muted-foreground">
                Names are applied to transcript labels and copied text.
              </p>
            </div>
            {isLoadingSpeakers && (
              <span className="text-xs text-muted-foreground">Loading…</span>
            )}
            {!isLoadingSpeakers && speakers.length > 0 && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => {
                  setCorrectionMode(value => !value);
                  setSelectedRowIds(new Set());
                }}
              >
                {correctionMode ? 'Done' : 'Correct rows'}
              </Button>
            )}
          </div>

          <div className="max-h-56 space-y-2 overflow-y-auto pr-1">
            {speakers.map((speaker) => {
              const isEditing = editingSpeakerId === speaker.speakerId;
              const isSaving = savingSpeakerId === speaker.speakerId;
              const transcriptSegmentLabel =
                speaker.segmentCount === 1 ? 'transcript segment' : 'transcript segments';

              return (
                <div
                  key={speaker.speakerId}
                  className="rounded-md border border-border bg-background/60 px-3 py-2"
                >
                  <div className="flex items-center gap-2">
                    <span
                      className="h-2.5 w-2.5 rounded-full border border-border shrink-0"
                      style={{ backgroundColor: speaker.speakerColor || '#94a3b8' }}
                    />

                    {isEditing ? (
                      <>
                        <Input
                          aria-label="Speaker name"
                          className="h-8 min-w-0 flex-1"
                          value={draftSpeakerLabel}
                          disabled={isSaving}
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
                        <label className="flex items-center gap-1 text-xs text-muted-foreground">
                          <input
                            type="checkbox"
                            checked={rememberVoice}
                            disabled={isSaving}
                            onChange={event => setRememberVoice(event.target.checked)}
                          />
                          Remember voice
                        </label>
                        <Button
                          size="sm"
                          onClick={() => void saveSpeakerLabel(speaker)}
                          disabled={isSaving}
                        >
                          Save
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={cancelEditingSpeaker}
                          disabled={isSaving}
                        >
                          Cancel
                        </Button>
                      </>
                    ) : (
                      <>
                        <div className="min-w-0 flex-1">
                          <p className="truncate text-sm font-medium text-foreground">
                            {speaker.speakerLabel}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {speaker.segmentCount} {transcriptSegmentLabel}
                          </p>
                        </div>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => startEditingSpeaker(speaker)}
                          disabled={savingSpeakerId !== null}
                        >
                          Rename
                        </Button>
                        {speakers.length > 1 && mergingSpeakerId !== speaker.speakerId && (
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => {
                              setMergingSpeakerId(speaker.speakerId);
                              setMergeTargetId('');
                            }}
                          >
                            Merge
                          </Button>
                        )}
                      </>
                    )}
                  </div>
                  {mergingSpeakerId === speaker.speakerId && (
                    <div className="mt-2 flex items-center gap-2">
                      <select
                        aria-label="Merge into speaker"
                        className="h-8 min-w-0 flex-1 rounded-md border border-border bg-background px-2 text-xs"
                        value={mergeTargetId}
                        onChange={event => setMergeTargetId(event.target.value)}
                      >
                        <option value="">Merge into…</option>
                        {speakers.filter(target => target.speakerId !== speaker.speakerId).map(target => (
                          <option key={target.speakerId} value={target.speakerId}>{target.speakerLabel}</option>
                        ))}
                      </select>
                      <Button size="sm" disabled={!mergeTargetId} onClick={() => void mergeSpeaker()}>Merge</Button>
                      <Button size="sm" variant="ghost" onClick={() => setMergingSpeakerId(null)}>Cancel</Button>
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
                  value={assignmentTargetId}
                  disabled={assignmentOverlap}
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
                    placeholder="New speaker name"
                    value={newSpeakerLabel}
                    onChange={event => setNewSpeakerLabel(event.target.value)}
                  />
                )}
                <label className="flex items-center gap-1 text-xs">
                  <input
                    type="checkbox"
                    checked={assignmentOverlap}
                    onChange={event => setAssignmentOverlap(event.target.checked)}
                  />
                  Multiple speakers
                </label>
                <Button size="sm" disabled={selectedRowIds.size === 0} onClick={() => void applyRowCorrection()}>
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
          selectedSegmentIds={correctionMode ? selectedRowIds : undefined}
          onToggleSegment={correctionMode ? toggleSelectedRow : undefined}
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
