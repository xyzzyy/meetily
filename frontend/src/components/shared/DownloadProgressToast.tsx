'use client';

import React, { useEffect, useState, useCallback, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { X, Check, ArrowBigDownDash } from 'lucide-react';
import { downloadProviders, checkDownloads, eventStatus, type DownloadEvent, type DownloadProvider, type DownloadStatus } from '@/lib/model-downloads';

interface DownloadProgress {
  provider: string;
  endpoint?: string | null;
  modelName: string;
  displayName: string;
  progress: number;
  downloadedMb: number;
  totalMb: number;
  speedMbps: number;
  status: DownloadStatus;
  unitLabel?: string;
  error?: string;
}

// Categorize error messages for better user experience
function categorizeError(error: string): string {
  const lowerError = error.toLowerCase();

  if (lowerError.includes('network') ||
    lowerError.includes('connection') ||
    lowerError.includes('timeout') ||
    lowerError.includes('failed to start download')) {
    return 'Network error - Check your internet connection';
  }

  if (lowerError.includes('status:') || lowerError.includes('http')) {
    return 'Server error - Download temporarily unavailable';
  }

  if (lowerError.includes('disk') ||
    lowerError.includes('write') ||
    lowerError.includes('file')) {
    return 'Storage error - Check available disk space';
  }

  if (lowerError.includes('invalid') || lowerError.includes('validation')) {
    return 'File validation failed - Please retry download';
  }

  // Fallback to original error
  return error;
}

// Custom toast component for download progress
function DownloadToastContent({
  download,
  onDismiss,
}: {
  download: DownloadProgress;
  onDismiss?: () => void;
}) {
  const isComplete = download.status === 'completed';
  const hasError = download.status === 'error';
  const isCancelled = download.status === 'cancelled';
  const unitLabel = download.unitLabel ?? 'MiB';
  const percent = Number.isFinite(download.progress) ? Math.min(100, Math.max(0, download.progress)) : 0;

  return (
    <div className="flex items-center gap-3 w-full max-w-sm bg-white dark:bg-ink-900 rounded-lg shadow-lg border border-gray-200 dark:border-ink-800 p-3 relative">

      {/* Icon */}
      <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${isComplete ? 'bg-green-100 dark:bg-green-500/15' : hasError ? 'bg-red-100 dark:bg-red-500/15' : isCancelled ? 'bg-gray-100 dark:bg-ink-800' : 'bg-gray-100 dark:bg-ink-800'
        }`}>
        {isComplete ? (
          <Check className="w-4 h-4 text-green-600 dark:text-green-400" />
        ) : hasError ? (
          <X className="w-4 h-4 text-red-600 dark:text-red-400" />
        ) : isCancelled ? (
          <X className="w-4 h-4 text-gray-600 dark:text-ink-300" />
        ) : (
          <ArrowBigDownDash className="size-5 text-gray-600 dark:text-ink-300 " />
        )}
      </div>

      {/* Content */}
      <div className="flex-1 min-w-0">
        <div className="flex items-center justify-between gap-2 mb-1">
          <p className="text-sm font-medium text-gray-900 dark:text-ink-100 truncate">
            {download.displayName}
          </p>
          <button type="button" onClick={onDismiss} aria-label="Close download notice"
            className="shrink-0 text-gray-500 dark:text-ink-400">
            <X className="h-4 w-4" />
          </button>
        </div>

        {hasError ? (
          <p className="text-xs text-red-600 dark:text-red-400">{download.error || 'Download failed'}</p>
        ) : isComplete ? (
          <p className="text-xs text-green-600 dark:text-green-400">Download complete</p>
        ) : isCancelled ? (
          <p className="text-xs text-gray-600 dark:text-ink-300">Download cancelled</p>
        ) : (
          <>
            {/* Progress bar */}
            <div role="progressbar" aria-label={`${download.displayName} download`}
              aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}
              className="w-full h-1.5 bg-gray-200 dark:bg-ink-800 rounded-full overflow-hidden mb-1.5">
              <div
                className="h-full bg-primary rounded-full transition-all duration-300"
                style={{ width: `${percent}%` }}
              />
            </div>

            {/* Progress text */}
            <div className="flex items-center justify-between text-xs text-gray-500 dark:text-ink-400">
              <span>
                {download.totalMb > 0 ? `${download.downloadedMb.toFixed(1)} / ${download.totalMb.toFixed(1)} ${unitLabel}` : 'Download in progress'}
              </span>
              <span className="flex items-center gap-1">
                {download.speedMbps > 0 && (
                  <span>{download.speedMbps.toFixed(1)} {unitLabel}/s</span>
                )}
                <span className="text-gray-900 dark:text-ink-100 font-medium">
                  {Math.round(percent)}%
                </span>
              </span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// One lifecycle for all model notices: events, status checks, dismissal, and cleanup.
export function useDownloadProgressToast() {
  const [downloads, setDownloads] = useState<Map<string, DownloadProgress>>(new Map());
  const current = useRef(downloads);
  const dismissed = useRef(new Set<string>());
  const cleanupTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const generations = useRef(new Map<string, number>());
  const generationCounter = useRef(0);
  const hasActiveDownloads = [...downloads.values()].some(download => download.status === 'downloading');

  const updateDownload = useCallback((provider: DownloadProvider, event: DownloadEvent, fallback: DownloadStatus) => {
    const modelName = event.modelName ?? event.model ?? provider.fixedModel;
    if (!modelName) return;
    const key = `${provider.id}:${modelName}`;
    const status = eventStatus(event, fallback);
    const old = current.current.get(key);
    const restart = old && old.status !== 'downloading' && status === 'downloading';
    if (restart || !old) {
      generations.current.set(key, ++generationCounter.current);
      dismissed.current.delete(key);
    }
    // Closing progress must not hide its result. A closed result stays closed.
    if (old?.status === 'downloading' && status !== 'downloading') dismissed.current.delete(key);
    const existing = restart ? undefined : old;
    const download: DownloadProgress = {
      provider: provider.id, modelName,
      displayName: `${provider.label}${provider.fixedModel ? '' : `: ${modelName}`}`,
      progress: status === 'completed' ? 100 : event.progress ?? event.percent ?? existing?.progress ?? 0,
      downloadedMb: event.downloaded_mb ?? (event.downloaded_bytes === undefined ? existing?.downloadedMb ?? 0 : event.downloaded_bytes / 1048576),
      totalMb: event.total_mb ?? (event.total_bytes === undefined ? existing?.totalMb ?? 0 : event.total_bytes / 1048576),
      speedMbps: status === 'downloading' ? event.speed_mbps ?? 0 : 0,
      endpoint: event.endpoint ?? existing?.endpoint,
      status,
      error: status === 'error' ? categorizeError(event.error ?? 'Download stopped before the model was ready') : undefined,
    };
    // Terminal events often omit sizes or send zero placeholders.
    if (status !== 'downloading' && existing) {
      download.downloadedMb = existing.downloadedMb;
      download.totalMb = existing.totalMb;
    }
    const next = new Map(current.current).set(key, download);
    current.current = next;
    setDownloads(next);
    // Publish here, not from a render effect. Sonner's Toaster resubscribes after each of its own
    // updates, so a notice sent from an effect in the same commit can reach no subscriber.
    if (!dismissed.current.has(key)) {
      const id = `download-${key}`;
      toast.custom(() => <DownloadToastContent download={download} onDismiss={() => {
        dismissed.current.add(key);
        toast.dismiss(id);
      }} />, {
        position: 'top-right', id,
        duration: status === 'downloading' ? Infinity : status === 'completed' ? 3000 : status === 'cancelled' ? 5000 : 10000,
      });
    }
    const timer = cleanupTimers.current.get(key);
    if (timer) clearTimeout(timer);
    cleanupTimers.current.delete(key);
    if (status !== 'downloading') {
      const delay = status === 'completed' ? 4000 : status === 'cancelled' ? 6000 : 11000;
      cleanupTimers.current.set(key, setTimeout(() => {
        toast.dismiss(`download-${key}`);
        const next = new Map(current.current);
        next.delete(key);
        current.current = next;
        setDownloads(next);
        dismissed.current.delete(key);
        cleanupTimers.current.delete(key);
        generations.current.delete(key);
      }, delay));
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    const subscriptions: Promise<() => void>[] = [];
    const ownedCleanupTimers = cleanupTimers.current;
    for (const provider of downloadProviders) {
      const subscribe = (name: string | undefined, status: DownloadStatus) => {
        if (!name) return;
        const subscription = listen<DownloadEvent>(name, async event => {
          if (disposed) return;
          const modelName = event.payload.modelName ?? event.payload.model ?? provider.fixedModel;
          const key = `${provider.id}:${modelName}`;
          const active = current.current.get(key);
          const attempt = generations.current.get(key);
          const nextStatus = eventStatus(event.payload, status);
          const duplicateError = active && nextStatus === 'error'
            && /already (?:in progress|downloading|running|being downloaded)/i.test(event.payload.error ?? '');
          if (duplicateError && active.status === 'completed') return;
          const resumedRetry = active && active.status !== 'downloading' && nextStatus === 'downloading'
            && (event.payload.progress ?? event.payload.percent) !== 0;
          // Only duplicate-request errors need worker checks. A real error is terminal.
          // Nonzero progress after a terminal notice needs an active worker to confirm a retry.
          if (duplicateError || resumedRetry) {
            try {
              const states = await checkDownloads(provider, [active]);
              if (disposed || generations.current.get(key) !== attempt || current.current.get(key) !== active) return;
              const verified = states.get(active.modelName);
              if (duplicateError && verified === 'completed') {
                updateDownload(provider, { modelName }, 'completed');
                return;
              }
              const downloading = verified === 'downloading';
              if (duplicateError && verified === undefined) return;
              if (resumedRetry ? !downloading : downloading) return;
            } catch (error) {
              console.warn(`Could not verify ${provider.id} download event`, error);
              return;
            }
          }
          if (!disposed) updateDownload(provider, event.payload, status);
        }).catch(error => {
          console.warn(`Could not listen for ${name}`, error);
          return () => {};
        });
        subscriptions.push(subscription);
      };
      subscribe(provider.progress, 'downloading');
      subscribe(provider.complete, 'completed');
      subscribe(provider.error, 'error');
      subscribe(provider.cancelled, 'cancelled');
      subscribe(provider.finished, 'completed');
    }
    return () => {
      disposed = true;
      subscriptions.forEach(subscription => { void subscription.then(unlisten => unlisten()).catch(error => console.warn('Could not remove download listener', error)); });
      ownedCleanupTimers.forEach(clearTimeout);
      ownedCleanupTimers.clear();
      current.current.forEach((_, key) => toast.dismiss(`download-${key}`));
    };
  }, [updateDownload]);

  useEffect(() => {
    if (!hasActiveDownloads) return;
    let disposed = false;
    // Keep the timer stable while progress events arrive. Query providers independently.
    const checking = new Set<string>();
    const timer = setInterval(async () => {
      await Promise.all(downloadProviders.map(async provider => {
        const pending = [...current.current.values()].filter(item => item.provider === provider.id && item.status === 'downloading');
        if (!pending.length || checking.has(provider.id)) return;
        checking.add(provider.id);
        const attempts = new Map(pending.map(item => [`${provider.id}:${item.modelName}`, generations.current.get(`${provider.id}:${item.modelName}`)]));
        try {
          const statuses = await checkDownloads(provider, pending);
          if (disposed) return;
          statuses.forEach((status, modelName) => {
            const key = `${provider.id}:${modelName}`;
            if (current.current.get(key)?.status === 'downloading' && generations.current.get(key) === attempts.get(key)) {
              if (status !== 'downloading') updateDownload(provider, { modelName }, status);
            }
          });
        } catch (error) {
          console.warn(`Could not check ${provider.id} download status`, error);
        } finally {
          checking.delete(provider.id);
        }
      }));
    }, 2000);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, [hasActiveDownloads, updateDownload]);

  return { downloads };
}

export function DownloadProgressToastProvider() {
  useDownloadProgressToast();
  return null;
}
