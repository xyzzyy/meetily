import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { Check, Download, HardDrive, Loader2, Trash2, X } from 'lucide-react';
import { toast } from 'sonner';
import {
  FunAsrAPI,
  FunAsrModelInfo,
  FunAsrModelStatus,
  funAsrProgress,
} from '@/lib/funasr';
import { Button } from './ui/button';
import { Progress } from './ui/progress';

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
  const [downloading, setDownloading] = useState<Set<string>>(new Set());
  const [downloadDetails, setDownloadDetails] = useState<
    Record<string, DownloadEvent>
  >({});
  const [savingModel, setSavingModel] = useState<string | null>(null);
  const savingRef = useRef(false);
  const downloadErrors = useRef(new Map<string, string>());

  const refresh = useCallback(async () => {
    await FunAsrAPI.init();
    const latest = await FunAsrAPI.getAvailableModels();
    setModels(latest);
    setDownloading(new Set(latest.filter(model => funAsrProgress(model.status) !== null).map(model => model.name)));
  }, []);

  const reportDownloadError = useCallback((modelName: string, error: string) => {
    if (downloadErrors.current.get(modelName) !== error) {
      downloadErrors.current.set(modelName, error);
      toast.error('FUN-ASR download failed', { description: error });
    }
  }, []);

  useEffect(() => {
    refresh()
      .catch((error) => {
        console.error('Failed to initialize FUN-ASR models:', error);
        toast.error('Failed to load FUN-ASR models', {
          description: String(error),
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
          setDownloading((current) => {
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
          toast.success('FUN-ASR model is ready. Select it to use it.');
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
          setDownloading((current) => {
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
  }, [refresh, reportDownloadError]);

  const saveSelection = async (modelName: string) => {
    await invoke('api_save_transcript_config', {
      provider: 'funasr',
      model: modelName,
      apiKey: null,
    });
  };

  const selectModel = async (modelName: string) => {
    if (savingRef.current) return;
    savingRef.current = true;
    setSavingModel(modelName);
    try {
      // Keep the displayed selection and modal unchanged until saving succeeds.
      if (autoSave) await saveSelection(modelName);
      onModelSelect?.(modelName);
    } catch (error) {
      toast.error('Could not save the transcription model', { description: String(error) });
    } finally {
      savingRef.current = false;
      setSavingModel(null);
    }
  };

  const downloadModel = async (modelName: string) => {
    downloadErrors.current.delete(modelName);
    setDownloading((current) => new Set(current).add(modelName));
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
      setDownloading(current => {
        const next = new Set(current);
        next.delete(modelName);
        return next;
      });
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
    try {
      const requested = await FunAsrAPI.cancelDownload(modelName);
      // A transfer may finish between rendering the Cancel button and clicking
      // it. Refresh that state without claiming that a transfer was cancelled.
      if (!requested) await refresh();
    } catch (error) {
      toast.error('Could not cancel the download', { description: String(error) });
    }
  };

  const deleteModel = async (model: FunAsrModelInfo) => {
    if (selectedModel === model.name && model.status === 'Available') {
      toast.error('Select another FUN-ASR model before deleting this one');
      return;
    }
    const accepted = await confirm(
      model.has_orphaned_encoder
        ? 'Delete the unused shared FUN-ASR encoder? No model uses it. You will need to download it again to install a FUN-ASR model.'
        : `Delete the files for ${model.display_name}, including any partial download? The shared encoder is kept while another FUN-ASR variant needs it.`,
      { title: model.has_orphaned_encoder ? 'Delete unused encoder?' : 'Delete FUN-ASR model?', kind: 'warning' }
    );
    if (!accepted) return;
    try {
      await FunAsrAPI.deleteModel(model.name);
      toast.success(model.has_orphaned_encoder
        ? 'Unused FUN-ASR encoder deleted'
        : `Files for ${model.display_name} deleted from this app`);
    } catch (error) {
      toast.error(`Could not delete ${model.display_name}`, { description: String(error) });
    } finally {
      await refresh().catch(error => toast.error('Could not refresh FUN-ASR models', { description: String(error) }));
    }
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 py-6 text-sm text-gray-500 dark:text-gray-400">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading FUN-ASR models…
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <div className="rounded-md border border-blue-100 dark:border-blue-800 bg-blue-50 dark:bg-blue-950/50 p-3 text-xs text-blue-900 dark:text-blue-200">
        <div className="flex items-center justify-between gap-2">
          <span>Shared FUN-ASR encoder · 469 MB</span>
          <span className="font-medium">
            {models.some((model) => model.encoder_available)
              ? 'Installed'
              : 'Downloaded with your first model'}
          </span>
        </div>
        <p className="mt-1">
          Installing another option downloads only its Qwen3 decoder.
        </p>
        {models.some(model => model.has_orphaned_encoder) && (
          <div className="mt-2 flex items-center justify-between gap-2">
            <span>No model uses this encoder. You can delete it to free disk space.</span>
            <Button variant="outline" size="sm" title="Delete unused encoder"
              className="hover:text-red-600 dark:hover:text-red-400 transition-colors"
              onClick={() => deleteModel(models.find(model => model.has_orphaned_encoder)!)}>
              <Trash2 className="mr-1 h-4 w-4" />
              Delete unused encoder
            </Button>
          </div>
        )}
      </div>
      {models.map((model) => {
        const available = model.status === 'Available';
        const progress = funAsrProgress(model.status);
        const isDownloading = downloading.has(model.name) || progress !== null;
        const statusError =
          typeof model.status === 'object' && 'Error' in model.status
            ? model.status.Error
            : null;
        const detail = downloadDetails[model.name];
        return (
          <div
            key={model.name}
            className={`rounded-lg border p-4 ${
              selectedModel === model.name
                ? 'border-blue-500 bg-blue-50/40 dark:bg-blue-950/40'
                : 'border-gray-200 dark:border-gray-800'
            }`}
          >
            <div className="flex items-start justify-between gap-3">
              <button
                type="button"
                className="min-w-0 flex-1 text-left"
                disabled={!available || savingModel !== null}
                onClick={() => selectModel(model.name)}
              >
                <div className="flex items-center gap-2">
                  <span className="font-medium text-gray-900 dark:text-gray-100">
                    {model.display_name}
                  </span>
                  {model.recommended && (
                    <span className="rounded-full bg-blue-100 dark:bg-blue-900/50 px-2 py-0.5 text-[10px] font-medium text-blue-700 dark:text-blue-300">
                      Default
                    </span>
                  )}
                  {savingModel === model.name && <Loader2 className="h-4 w-4 animate-spin" />}
                  {selectedModel === model.name && available && (
                    <Check className="h-4 w-4 text-blue-600 dark:text-blue-400" />
                  )}
                </div>
                <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{model.description}</p>
                <p className="mt-1 flex items-center gap-1 text-xs text-gray-500 dark:text-gray-400">
                  <HardDrive className="h-3 w-3" />
                  {model.decoder_size_mb} MB decoder · {model.size_mb} MB first install
                </p>
                {selectedModel === model.name && available && (
                  <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                    The selected model can't be deleted. Choose another model first.
                  </p>
                )}
              </button>
              {available ? (
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => deleteModel(model)}
                  className="text-gray-400 hover:text-red-600 dark:hover:text-red-400 transition-colors"
                  disabled={selectedModel === model.name || savingModel !== null}
                  title={
                    selectedModel === model.name
                      ? 'Select another model before deleting this one'
                      : 'Delete model'
                  }
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              ) : isDownloading ? (
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => cancelDownload(model.name)}
                  title="Cancel download"
                >
                  <X className="h-4 w-4" />
                </Button>
              ) : (
                <div className="flex items-center gap-1">
                  <Button variant="outline" size="sm" onClick={() => downloadModel(model.name)}>
                    <Download className="mr-1 h-4 w-4" />
                    {model.has_partial_download ? 'Resume' : 'Download'}
                  </Button>
                  {model.has_partial_download && (
                    <Button variant="ghost" size="icon" onClick={() => deleteModel(model)} title="Delete partial download"
                      className="text-gray-400 hover:text-red-600 dark:hover:text-red-400 transition-colors">
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  )}
                </div>
              )}
            </div>
            {progress !== null && (
              <div className="mt-3 space-y-1">
                <Progress value={progress} />
                <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400">
                  <span>
                    {detail?.phase === 'encoder'
                      ? 'Downloading shared encoder'
                      : 'Downloading decoder'}
                  </span>
                  <span>
                    {detail?.downloaded_bytes !== undefined &&
                    detail?.total_bytes !== undefined
                      ? `${Math.round(detail.downloaded_bytes / 1_048_576)} / ${Math.round(
                          detail.total_bytes / 1_048_576
                        )} MB · `
                      : ''}
                    {progress}%
                  </span>
                </div>
              </div>
            )}
            {statusError && (
              <p className="mt-2 text-xs text-red-600 dark:text-red-400">{statusError}</p>
            )}
          </div>
        );
      })}
    </div>
  );
}
