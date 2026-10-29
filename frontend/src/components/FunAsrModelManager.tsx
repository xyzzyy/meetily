import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { Trash2 } from 'lucide-react';
import { toast } from 'sonner';
import {
  FunAsrAPI,
  FunAsrModelInfo,
  FunAsrModelStatus,
  funAsrProgress,
  funAsrStorageNote,
} from '@/lib/funasr';
import { useModelSelection } from '@/hooks/useModelSelection';
import { Button } from './ui/button';
import {
  ModelListError,
  ModelListSkeleton,
  SelectedModelNote,
  TranscriptionModelCard,
  errorMessage,
  modelCardStatus,
} from './TranscriptionModelCard';

interface FunAsrModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  autoSave?: boolean;
}

interface DownloadEvent {
  modelName: string;
  progress: number;
  phase?: string;
  status?: string;
  downloaded_bytes?: number;
  total_bytes?: number;
}

export function FunAsrModelManager({
  selectedModel,
  onModelSelect,
  autoSave = false,
}: FunAsrModelManagerProps) {
  const [models, setModels] = useState<FunAsrModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Cancel is a request; the transfer row stays until the worker stops.
  const [cancelling, setCancelling] = useState<Set<string>>(new Set());
  const [downloadDetails, setDownloadDetails] = useState<
    Record<string, DownloadEvent>
  >({});
  const downloadErrors = useRef(new Map<string, string>());
  const displayNames = useRef(new Map<string, string>());
  const displayName = useCallback(
    (modelName: string) => displayNames.current.get(modelName) ?? modelName,
    []
  );
  const { savingModel, selectModel } = useModelSelection({
    provider: 'funasr',
    autoSave,
    onModelSelect,
    displayName,
  });

  const refresh = useCallback(async () => {
    await FunAsrAPI.init();
    const latest = await FunAsrAPI.getAvailableModels();
    latest.forEach(model => displayNames.current.set(model.name, model.display_name));
    setModels(latest);
    const active = new Set(latest.filter(model => funAsrProgress(model.status) !== null).map(model => model.name));
    setCancelling(current => new Set([...current].filter(name => active.has(name))));
  }, []);

  const reportDownloadError = useCallback((modelName: string, error: string) => {
    if (downloadErrors.current.get(modelName) !== error) {
      downloadErrors.current.set(modelName, error);
      toast.error(`Failed to download ${displayName(modelName)}`, { description: error });
    }
  }, [displayName]);

  useEffect(() => {
    refresh()
      .catch((error) => {
        console.error('Failed to initialize FUN-ASR models:', error);
        setLoadError(errorMessage(error));
        toast.error('Failed to load transcription models', {
          description: errorMessage(error),
        });
      })
      .finally(() => setLoading(false));
  }, [refresh]);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;
    const setup = async () => {
      const progress = await listen<DownloadEvent>(
        'funasr-model-download-progress',
        (event) => {
          const { modelName, progress, status } = event.payload;
          // Cancel is a request. Keep the active controls until the worker
          // stops and sends its terminal event.
          if (status === 'cancelled') return;
          setDownloadDetails((current) => ({
            ...current,
            [modelName]: event.payload,
          }));
          setModels((current) =>
            current.map((model) =>
              model.name === modelName
                ? {
                    ...model,
                    status: { Downloading: { progress } } as FunAsrModelStatus,
                  }
                : model
            )
          );
        }
      );
      if (disposed) { progress(); return; }
      unlisteners.push(progress);
      const complete = await listen<{ modelName: string }>(
        'funasr-model-download-complete',
        async (event) => {
          setCancelling((current) => {
            const next = new Set(current);
            next.delete(event.payload.modelName);
            return next;
          });
          setDownloadDetails((current) => {
            const next = { ...current };
            delete next[event.payload.modelName];
            return next;
          });
          await refresh().catch((error) => toast.error('Could not refresh FUN-ASR models', { description: String(error) }));
          // Keep the saved model until the user chooses the new one.
          toast.success(`${displayName(event.payload.modelName)} is ready`, {
            description: 'Select it to use it for transcription.',
          });
        }
      );
      if (disposed) { complete(); return; }
      unlisteners.push(complete);
      const error = await listen<{
        modelName: string;
        error: string;
        status?: string;
      }>(
        'funasr-model-download-error',
        (event) => {
          setCancelling((current) => {
            const next = new Set(current);
            next.delete(event.payload.modelName);
            return next;
          });
          setDownloadDetails((current) => {
            const next = { ...current };
            delete next[event.payload.modelName];
            return next;
          });
          // A rejected duplicate request can arrive while the first download
          // still runs. Read the backend state instead of hiding that download.
          refresh().catch((error) => toast.error('Could not refresh FUN-ASR models', { description: String(error) }));
          if (event.payload.status !== 'cancelled') {
            reportDownloadError(event.payload.modelName, event.payload.error);
          }
        }
      );
      if (disposed) {
        error();
      } else {
        unlisteners.push(error);
      }
    };
    setup().catch((error) => toast.error('Could not listen for FUN-ASR downloads', { description: String(error) }));
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [refresh, reportDownloadError, displayName]);

  const clearCancelling = (modelName: string) => {
    setCancelling(current => {
      const next = new Set(current);
      next.delete(modelName);
      return next;
    });
  };

  const downloadModel = async (modelName: string) => {
    downloadErrors.current.delete(modelName);
    clearCancelling(modelName);
    setModels((current) =>
      current.map((model) =>
        model.name === modelName
          ? { ...model, status: { Downloading: { progress: 0 } } }
          : model
      )
    );
    try {
      await FunAsrAPI.downloadModel(modelName);
    } catch (error) {
      clearCancelling(modelName);
      const message = error instanceof Error ? error.message : String(error);
      const cancelled = message === 'FUN-ASR model download cancelled';
      // The worker also rejects the command on a normal cancellation. Show
      // actual failures even if the user requested Cancel before they occurred.
      if (!cancelled) reportDownloadError(modelName, message);
      try { await refresh(); } catch (refreshError) {
        setModels(current => current.map(model => model.name === modelName
          ? { ...model, status: cancelled ? 'Missing' : { Error: message } } : model));
        console.error('Could not refresh FUN-ASR models:', refreshError);
      }
    }
  };

  const cancelDownload = async (modelName: string) => {
    // Mark the request before it is sent: the worker's terminal event can
    // arrive before the command returns, and it must be able to clear this.
    setCancelling(current => new Set(current).add(modelName));
    try {
      const requested = await FunAsrAPI.cancelDownload(modelName);
      // A transfer may finish between rendering the Cancel button and clicking
      // it. Refresh that state without claiming that a transfer was cancelled.
      if (!requested) {
        clearCancelling(modelName);
        await refresh();
      }
    } catch (error) {
      clearCancelling(modelName);
      toast.error('Could not cancel the download', { description: String(error) });
    }
  };

  const deletionPendingRef = useRef(false);

  const deleteModel = async (model: FunAsrModelInfo) => {
    // A ready selected model must be replaced before it is removed. Damaged or
    // partial files of the selected model can still be deleted: that selection
    // was already unusable, and SelectedModelNote says so.
    if (selectedModel === model.name && model.status === 'Available') {
      toast.error('Select another model before deleting this one');
      return;
    }
    if (deletionPendingRef.current) return;
    deletionPendingRef.current = true;
    try {
      const accepted = await confirm(
        model.has_orphaned_encoder
          ? 'Delete the unused shared FUN-ASR encoder? No model uses it. You will need to download it again to install a FUN-ASR model.'
          : `Delete the Branch files for ${model.display_name}, including partial downloads? Files from official Meetily stay unchanged. The Branch encoder can also be removed if no Branch decoder needs it.`,
        {
          title: model.has_orphaned_encoder ? 'Delete unused encoder?' : 'Delete transcription model?',
          kind: 'warning',
          okLabel: 'Delete',
          cancelLabel: 'Cancel',
        }
      );
      if (!accepted) return;
      await FunAsrAPI.deleteModel(model.name);
      toast.success(model.has_orphaned_encoder
        ? 'Unused FUN-ASR encoder deleted'
        : `Branch files for ${model.display_name} deleted`, { description: 'Files from official Meetily stay unchanged' });
      await refresh().catch(error => toast.error('Could not refresh FUN-ASR models', { description: String(error) }));
    } catch (error) {
      toast.error(`Failed to delete ${model.display_name}`, { description: errorMessage(error) });
      await refresh().catch(error => toast.error('Could not refresh FUN-ASR models', { description: String(error) }));
    } finally {
      deletionPendingRef.current = false;
    }
  };

  if (loading) {
    return <ModelListSkeleton rows={2} />;
  }

  if (loadError) {
    return <ModelListError message={loadError} />;
  }

  const orphanedEncoderModel = models.find(model => model.has_orphaned_encoder);

  return (
    <div className="space-y-3">
      {/* FUN-ASR variants share one encoder, so its state is shown once. */}
      <div className="rounded-lg border border-gray-200 dark:border-ink-800 bg-gray-50 dark:bg-ink-800/50 p-3 text-xs text-gray-600 dark:text-ink-300">
        <div className="flex items-center justify-between gap-2">
          <span className="font-medium text-gray-900 dark:text-ink-100">Shared speech encoder · 469 MB</span>
          {models.some((model) => model.encoder_available) ? (
            <span className="flex items-center gap-1.5 font-medium text-green-600 dark:text-green-400">
              <span className="h-2 w-2 rounded-full bg-green-500" aria-hidden="true" />
              Installed
            </span>
          ) : (
            <span>Downloaded with your first model</span>
          )}
        </div>
        <p className="mt-1">
          Installing another option downloads only its Qwen3 decoder.
        </p>
        {orphanedEncoderModel && (
          <div className="mt-2 flex items-center justify-between gap-2">
            <span>No model uses this encoder. You can delete it to free disk space.</span>
            <Button variant="outline" size="sm" title="Delete unused encoder"
              className="hover:text-red-600 dark:hover:text-red-400 transition-colors"
              onClick={() => deleteModel(orphanedEncoderModel)}>
              <Trash2 className="h-4 w-4" />
              Delete unused encoder
            </Button>
          </div>
        )}
      </div>
      {models.map((model) => {
        const detail = downloadDetails[model.name];
        return (
          <TranscriptionModelCard
            key={model.name}
            displayName={model.display_name}
            canDelete={!model.decoder_is_shared || model.has_partial_download}
            storageNote={funAsrStorageNote(model)}
            description={model.description}
            details={[`${model.decoder_size_mb} MB decoder`, `${model.size_mb} MB first install`]}
            recommended={model.recommended}
            selected={selectedModel === model.name}
            saving={savingModel === model.name}
            selectionLocked={savingModel !== null}
            status={modelCardStatus(model.status, {
              // The terminal event can arrive before the cancel request returns.
              cancelling: cancelling.has(model.name) && funAsrProgress(model.status) !== null,
              partial: model.has_partial_download,
              label: detail?.phase === 'encoder' ? 'Downloading shared encoder' : 'Downloading decoder',
              detail: () => detail?.downloaded_bytes !== undefined && detail?.total_bytes !== undefined
                ? `${Math.round(detail.downloaded_bytes / 1_048_576)} / ${Math.round(detail.total_bytes / 1_048_576)} MB`
                : undefined,
            })}
            onSelect={() => selectModel(model.name)}
            onDownload={() => downloadModel(model.name)}
            onCancel={() => cancelDownload(model.name)}
            onDelete={() => deleteModel(model)}
          />
        );
      })}

      <SelectedModelNote
        displayName={selectedModel ? displayName(selectedModel) : undefined}
        ready={models.some(model => model.name === selectedModel && model.status === 'Available')}
      />
    </div>
  );
}
