import React, { type ReactNode } from 'react';
import { Check, Download, Loader2, RotateCcw, Trash2 } from 'lucide-react';
import { Button } from './ui/button';
import { Progress } from './ui/progress';

/** What a model card shows, independent of the engine that reports it. */
export type ModelCardStatus =
  | { kind: 'available' }
  | { kind: 'missing'; partial?: boolean }
  | { kind: 'downloading'; progress: number; cancelling?: boolean; label?: string; detail?: string }
  | { kind: 'error'; message: string; partial?: boolean }
  | { kind: 'corrupted' };

/** The status shape shared by the Whisper, Parakeet and FUN-ASR engines. */
type EngineModelStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string }
  | { Corrupted: unknown };

interface CardStatusOptions {
  cancelling?: boolean;
  partial?: boolean;
  label?: string;
  detail?: (progress: number) => string | undefined;
}

export function modelCardStatus(status: EngineModelStatus, options: CardStatusOptions = {}): ModelCardStatus {
  const { cancelling = false, partial, label, detail } = options;
  const progress = typeof status === 'object' && 'Downloading' in status ? status.Downloading.progress : null;
  // A cancellation request keeps the transfer row until the engine reports the outcome.
  if (progress !== null || cancelling) {
    const value = progress ?? 0;
    return { kind: 'downloading', progress: value, cancelling, label, detail: detail?.(value) };
  }
  if (status === 'Available') return { kind: 'available' };
  if (typeof status === 'object' && 'Error' in status) return { kind: 'error', message: status.Error, partial };
  if (typeof status === 'object' && 'Corrupted' in status) return { kind: 'corrupted' };
  return { kind: 'missing', partial };
}

interface TranscriptionModelCardProps {
  displayName: string;
  isShared?: boolean;
  storageNote?: string;
  canDelete?: boolean;
  icon?: string;
  description?: string;
  details?: string[];
  badges?: ReactNode;
  recommended?: boolean;
  selected: boolean;
  /** This model's selection is being saved. */
  saving?: boolean;
  /** Another selection is being saved. */
  selectionLocked?: boolean;
  status: ModelCardStatus;
  onSelect: () => void | Promise<void>;
  onDownload: () => void | Promise<void>;
  onCancel: () => void | Promise<void>;
  onDelete: () => void | Promise<void>;
}

const deleteIconClass = 'text-gray-400 dark:text-ink-400 hover:text-red-600 dark:hover:text-red-400';

export function TranscriptionModelCard({
  displayName,
  isShared,
  storageNote,
  canDelete,
  icon,
  description,
  details = [],
  badges,
  recommended = false,
  selected,
  saving = false,
  selectionLocked = false,
  status,
  onSelect,
  onDownload,
  onCancel,
  onDelete,
}: TranscriptionModelCardProps) {
  const available = status.kind === 'available';
  const deletionAllowed = canDelete ?? !isShared;
  const sourceNote = storageNote ?? (isShared === undefined ? undefined
    : isShared ? 'Shared from official Meetily. Read-only in Branch.'
    : available || status.kind === 'corrupted' ? 'Saved in Meetily Branch.' : 'Downloads are saved in Meetily Branch.');
  const isSelected = selected && available;
  const partial = (status.kind === 'missing' || status.kind === 'error') && status.partial;

  return (
    <div
      className={`rounded-lg border-2 p-4 transition-colors ${
        isSelected
          ? 'border-blue-500 bg-blue-50 dark:bg-blue-500/10'
          : available
            ? 'border-gray-200 dark:border-ink-800 hover:border-gray-300 dark:hover:border-ink-700 bg-white dark:bg-ink-900'
            : 'border-gray-200 dark:border-ink-800 bg-gray-50 dark:bg-ink-800/50'
      }`}
    >
      <div className="flex items-start justify-between gap-3">
        <button
          type="button"
          className="min-w-0 flex-1 rounded-md text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 disabled:cursor-default"
          disabled={!available || selectionLocked}
          aria-pressed={isSelected}
          onClick={onSelect}
        >
          <div className="flex flex-wrap items-center gap-2">
            {icon && <span className="text-xl leading-none" aria-hidden="true">{icon}</span>}
            <span className="font-semibold text-gray-900 dark:text-ink-100">{displayName}</span>
            {recommended && (
              <span className="rounded-full bg-blue-100 dark:bg-blue-500/15 px-2 py-0.5 text-xs font-medium text-blue-700 dark:text-blue-300">
                Recommended
              </span>
            )}
            {badges}
            {isSelected && (
              <span className="flex items-center gap-1 rounded-full bg-blue-600 dark:bg-blue-500/20 px-2 py-0.5 text-xs font-medium text-white dark:text-blue-200">
                <Check className="h-3 w-3" aria-hidden="true" />
                Selected
              </span>
            )}
            {saving && <Loader2 className="h-4 w-4 animate-spin text-gray-500 dark:text-ink-400" aria-label="Saving selection" />}
          </div>
          {description && (
            <p className="mt-1 text-sm text-gray-600 dark:text-ink-300">{description}</p>
          )}
          {details.length > 0 && (
            <p className="mt-1 text-xs text-gray-500 dark:text-ink-400">{details.join(' · ')}</p>
          )}
        </button>

        <div className="flex shrink-0 items-center gap-2">
          {available && (
            <>
              <span className="flex items-center gap-1.5 text-xs font-medium text-green-600 dark:text-green-400">
                <span className="h-2 w-2 rounded-full bg-green-500" aria-hidden="true" />
                Ready
              </span>
              {deletionAllowed && <Button
                variant="ghost"
                size="icon"
                className={`h-8 w-8 ${deleteIconClass} ${isSelected ? 'opacity-50' : ''}`}
                aria-disabled={isSelected || selectionLocked}
                aria-label={`Delete ${displayName}`}
                title={isSelected ? 'Select another model before deleting this one' : 'Delete model'}
                onClick={selectionLocked ? undefined : onDelete}
              >
                <Trash2 className="h-4 w-4" />
              </Button>}
            </>
          )}

          {(status.kind === 'missing' || status.kind === 'error') && (
            <>
              {partial && deletionAllowed && (
                <Button
                  variant="ghost"
                  size="icon"
                  className={`h-8 w-8 ${deleteIconClass}`}
                  aria-label={`Delete partial download of ${displayName}`}
                  title="Delete partial download"
                  onClick={onDelete}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              )}
              <Button variant={status.kind === 'error' ? 'red' : 'blue'} size="sm" onClick={onDownload}>
                {status.kind === 'error' ? <RotateCcw className="h-4 w-4" /> : <Download className="h-4 w-4" />}
                {status.kind === 'error' ? 'Retry' : partial ? 'Resume' : 'Download'}
              </Button>
            </>
          )}

          {status.kind === 'corrupted' && (
            <>
              {deletionAllowed && <Button
                variant="ghost"
                size="icon"
                className={`h-8 w-8 ${deleteIconClass}`}
                aria-label={`Delete damaged files of ${displayName}`}
                title="Delete damaged files"
                onClick={onDelete}
              >
                <Trash2 className="h-4 w-4" />
              </Button>}
              <Button variant="blue" size="sm" onClick={onDownload}>
                <RotateCcw className="h-4 w-4" />
                Re-download
              </Button>
            </>
          )}
        </div>
      </div>

      {sourceNote && status.kind !== 'downloading' && (
        <p className="mt-2 text-xs text-gray-500 dark:text-ink-400">
          {sourceNote}
        </p>
      )}

      {status.kind === 'downloading' && (
        <div className="mt-3 space-y-1.5 border-t border-gray-200 dark:border-ink-800 pt-3">
          <div className="flex items-center justify-between gap-2 text-xs">
            <span className="font-medium text-blue-600 dark:text-blue-400">
              {status.cancelling ? 'Cancelling…' : status.label ?? 'Downloading…'}
            </span>
            <div className="flex items-center gap-2">
              {status.cancelling ? (
                <span className="text-gray-500 dark:text-ink-400">Cancellation requested</span>
              ) : (
                <>
                  <span className="text-gray-500 dark:text-ink-400">
                    {status.detail ? `${status.detail} · ` : ''}
                    {Math.round(status.progress)}%
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 px-2 text-gray-600 dark:text-ink-300 hover:text-red-600 dark:hover:text-red-400 hover:bg-red-50 dark:hover:bg-red-500/10"
                    title="Cancel download"
                    onClick={onCancel}
                  >
                    Cancel
                  </Button>
                </>
              )}
            </div>
          </div>
          <Progress
            value={status.progress}
            className="bg-gray-200 dark:bg-ink-800 [&>div]:bg-blue-600 dark:[&>div]:bg-blue-500"
          />
        </div>
      )}

      {status.kind === 'error' && (
        <p className="mt-2 text-xs text-red-600 dark:text-red-400" role="alert">{status.message}</p>
      )}
      {status.kind === 'corrupted' && (
        <p className="mt-2 text-xs text-orange-700 dark:text-orange-300">
          {isShared ? 'The shared model files are damaged. Download a copy for Branch.' : 'The model files are incomplete or damaged. Delete them or download the model again.'}
        </p>
      )}
    </div>
  );
}

export function ModelListSkeleton({ rows = 2 }: { rows?: number }) {
  return (
    <div className="animate-pulse space-y-3" aria-busy="true" aria-label="Loading models">
      {Array.from({ length: rows }, (_, index) => (
        <div key={index} className="h-20 rounded-lg bg-gray-100 dark:bg-ink-800" />
      ))}
    </div>
  );
}

export function ModelListError({ message }: { message: string }) {
  return (
    <div className="rounded-lg border border-red-200 dark:border-red-500/30 bg-red-50 dark:bg-red-500/10 p-4" role="alert">
      <p className="text-sm text-red-800 dark:text-red-200">Failed to load models</p>
      <p className="mt-1 text-xs text-red-600 dark:text-red-400">{message}</p>
    </div>
  );
}

/** Names the saved model, and says so when it cannot be used as it is. */
export function SelectedModelNote({ displayName, ready }: { displayName?: string; ready: boolean }) {
  if (!displayName) return null;
  if (!ready) {
    return (
      <p className="pt-2 text-center text-xs text-orange-700 dark:text-orange-300" role="status">
        {displayName} is selected but not ready. Download it or choose another model.
      </p>
    );
  }
  return (
    <p className="pt-2 text-center text-xs text-gray-500 dark:text-ink-400">
      Using {displayName} for transcription
    </p>
  );
}

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
