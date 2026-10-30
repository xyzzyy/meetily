"use client";

import { useState, useCallback, useEffect } from 'react';
import { Button } from '@/components/ui/button';
import { ButtonGroup } from '@/components/ui/button-group';
import { Copy, FolderOpen, RefreshCw, Undo2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { RetranscribeDialog } from './RetranscribeDialog';
import { useConfig } from '@/contexts/ConfigContext';


interface TranscriptButtonGroupProps {
  transcriptCount: number;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}


export function TranscriptButtonGroup({
  transcriptCount,
  onCopyTranscript,
  onOpenMeetingFolder,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptButtonGroupProps) {
  const { betaFeatures } = useConfig();
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [undoReviewId, setUndoReviewId] = useState<string | null>(null);
  const [undoing, setUndoing] = useState(false);
  const refreshUndo = useCallback(async () => {
    if (!meetingId) { setUndoReviewId(null); return; }
    const result = await invoke<{ id: string; applied: boolean } | null>('get_enhancement_review', { meetingId });
    setUndoReviewId(result?.applied ? result.id : null);
  }, [meetingId]);
  useEffect(() => {
    setUndoReviewId(null);
    let active = true;
    if (meetingId) invoke<{ id: string; applied: boolean } | null>('get_enhancement_review', { meetingId })
      .then(result => { if (active) setUndoReviewId(result?.applied ? result.id : null); })
      .catch(() => { if (active) setUndoReviewId(null); });
    return () => { active = false; };
  }, [meetingId]);

  const handleRetranscribeComplete = useCallback(async () => {
    await refreshUndo().catch(() => setUndoReviewId(null));
    // Refetch transcripts to show the updated data
    if (onRefetchTranscripts) {
      await onRefetchTranscripts();
    }
  }, [onRefetchTranscripts, refreshUndo]);

  const handleUndo = async () => {
    if (!meetingId || !undoReviewId || undoing) return;
    setUndoing(true);
    try {
      const warning = await invoke<string | null>('undo_enhancement', { meetingId, reviewId: undoReviewId });
      await handleRetranscribeComplete();
      if (warning) toast.warning(warning); else toast.success('Enhancement undone.');
    } catch (error) { toast.error(String(error)); }
    finally { setUndoing(false); }
  };

  return (
    <div className="flex items-center justify-center w-full gap-2">
      <ButtonGroup>
        <Button
          variant="outline"
          size="sm"
          className="px-2 @[22rem]:px-3"
          onClick={() => onCopyTranscript()}
          disabled={transcriptCount === 0}
          title={transcriptCount === 0 ? 'No transcript available' : 'Copy Transcript'}
        >
          <Copy />
          <span className="hidden @[22rem]:inline">Copy</span>
        </Button>

        <Button
          size="sm"
          variant="outline"
          className="px-2 @[22rem]:px-4"
          onClick={() => onOpenMeetingFolder()}
          title="Open Recording Folder"
        >
          <FolderOpen className="@[22rem]:mr-2" size={18} />
          <span className="hidden @[22rem]:inline">Recording</span>
        </Button>

        {undoReviewId && (
          <Button size="sm" variant="outline" onClick={handleUndo} disabled={undoing} title="Undo last enhancement" className="px-2 @[22rem]:px-3">
            <Undo2 />
            <span className="hidden @[22rem]:inline">{undoing ? 'Undoing...' : 'Undo'}</span>
          </Button>
        )}

        {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
          <Button
            size="sm"
            variant="outline"
            className="bg-gradient-to-r from-blue-50 dark:from-blue-500/10 to-purple-50 dark:to-purple-500/10 hover:from-blue-100 dark:hover:from-blue-500/15 hover:to-purple-100 dark:hover:to-purple-500/15 border-blue-200 dark:border-blue-500/30 px-2 @[22rem]:px-4"
            onClick={() => setShowRetranscribeDialog(true)}
            title="Retranscribe to enhance your recorded audio"
          >
            <RefreshCw className="@[22rem]:mr-2" size={18} />
            <span className="hidden @[22rem]:inline">Enhance</span>
          </Button>
        )}
      </ButtonGroup>

      {betaFeatures.importAndRetranscribe && meetingId && meetingFolderPath && (
        <RetranscribeDialog
          open={showRetranscribeDialog}
          onOpenChange={setShowRetranscribeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
        />
      )}
    </div>
  );
}
