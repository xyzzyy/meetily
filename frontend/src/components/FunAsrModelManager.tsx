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
  const selectRef = useRef(onModelSelect);
  const autoSaveRef = useRef(autoSave);

  useEffect(() => {
    selectRef.current = onModelSelect;
    autoSaveRef.current = autoSave;
  }, [onModelSelect, autoSave]);

  const refresh = useCallback(async () => {
    await FunAsrAPI.init();
    setModels(await FunAsrAPI.getAvailableModels());
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
          setDownloadDetails((current) => ({
            ...current,
            [modelName]: event.payload,
          }));
          if (status === 'cancelled') {
            setDownloading((current) => {
              const next = new Set(current);
              next.delete(modelName);
              return next;
            });
          }
          setModels((current) =>
            current.map((model) =>
              model.name === modelName
                ? {
                    ...model,
                    status:
                      status === 'cancelled'
                        ? 'Missing'
                        : ({ Downloading: { progress } } as FunAsrModelStatus),
                  }
                : model
            )
          );
        }
      );
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
          await refresh();
          selectRef.current?.(event.payload.modelName);
          if (autoSaveRef.current) {
            await saveSelection(event.payload.modelName);
          }
          toast.success('FUN-ASR model is ready');
        }
      );
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
          if (event.payload.status === 'cancelled') {
            refresh().catch(console.error);
          } else {
            setModels((current) =>
              current.map((model) =>
                model.name === event.payload.modelName
                  ? { ...model, status: { Error: event.payload.error } }
                  : model
              )
            );
            toast.error('FUN-ASR download failed', {
              description: event.payload.error,
            });
          }
        }
      );
      if (disposed) {
        progress();
        complete();
        error();
      } else {
        unlisteners.push(progress, complete, error);
      }
    };
    setup();
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [refresh]);

  const saveSelection = async (modelName: string) => {
    await invoke('api_save_transcript_config', {
      provider: 'funasr',
      model: modelName,
      apiKey: null,
    });
  };

  const selectModel = async (modelName: string) => {
    onModelSelect?.(modelName);
    if (autoSave) await saveSelection(modelName);
  };

  const downloadModel = async (modelName: string) => {
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
    } catch {
      // The backend event contains the actionable error.
    }
  };

  const cancelDownload = async (modelName: string) => {
    await FunAsrAPI.cancelDownload(modelName);
  };

  const deleteModel = async (model: FunAsrModelInfo) => {
    const accepted = await confirm(
      `Delete ${model.display_name}? The shared encoder is kept while another FUN-ASR variant is installed.`,
      { title: 'Delete FUN-ASR model?', kind: 'warning' }
    );
    if (!accepted) return;
    await FunAsrAPI.deleteModel(model.name);
    await refresh();
    toast.success(`${model.display_name} deleted`);
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 py-6 text-sm text-gray-500">
        <Loader2 className="h-4 w-4 animate-spin" />
        Loading FUN-ASR models…
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <div className="rounded-md border border-blue-100 bg-blue-50 p-3 text-xs text-blue-900">
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
                ? 'border-blue-500 bg-blue-50/40'
                : 'border-gray-200'
            }`}
          >
            <div className="flex items-start justify-between gap-3">
              <button
                type="button"
                className="min-w-0 flex-1 text-left"
                disabled={!available}
                onClick={() => selectModel(model.name)}
              >
                <div className="flex items-center gap-2">
                  <span className="font-medium text-gray-900">
                    {model.display_name}
                  </span>
                  {model.recommended && (
                    <span className="rounded-full bg-blue-100 px-2 py-0.5 text-[10px] font-medium text-blue-700">
                      Default
                    </span>
                  )}
                  {selectedModel === model.name && available && (
                    <Check className="h-4 w-4 text-blue-600" />
                  )}
                </div>
                <p className="mt-1 text-xs text-gray-500">{model.description}</p>
                <p className="mt-1 flex items-center gap-1 text-xs text-gray-500">
                  <HardDrive className="h-3 w-3" />
                  {model.decoder_size_mb} MB decoder · {model.size_mb} MB first install
                </p>
              </button>
              {available ? (
                <Button
                  variant="ghost"
                  size="icon"
                  onClick={() => deleteModel(model)}
                  title="Delete model"
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
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => downloadModel(model.name)}
                >
                  <Download className="mr-1 h-4 w-4" />
                  Download
                </Button>
              )}
            </div>
            {progress !== null && (
              <div className="mt-3 space-y-1">
                <Progress value={progress} />
                <div className="flex justify-between text-xs text-gray-500">
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
              <p className="mt-2 text-xs text-red-600">{statusError}</p>
            )}
          </div>
        );
      })}
    </div>
  );
}
