import React, { useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { toast } from 'sonner';
import {
  ModelStatus,
  ParakeetAPI,
  ParakeetDownloadProgressEvent,
  ParakeetModelInfo,
  formatFileSize,
  getModelDisplayInfo,
  getModelDisplayName
} from '../lib/parakeet';
import { useModelSelection } from '@/hooks/useModelSelection';
import {
  ModelListError,
  ModelListSkeleton,
  SelectedModelNote,
  TranscriptionModelCard,
  errorMessage,
  modelCardStatus
} from './TranscriptionModelCard';

const RECOMMENDED_MODEL = 'parakeet-tdt-0.6b-v3-int8';

interface ParakeetModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  className?: string;
  autoSave?: boolean;
}

export function ParakeetModelManager({
  selectedModel,
  onModelSelect,
  className = '',
  autoSave = false
}: ParakeetModelManagerProps) {
  const [models, setModels] = useState<ParakeetModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);
  const [listenersReady, setListenersReady] = useState(false);
  const [downloadingModels, setDownloadingModels] = useState<Set<string>>(new Set());
  const [cancellingModels, setCancellingModels] = useState<Set<string>>(new Set());
  const { savingModel, selectModel } = useModelSelection({
    provider: 'parakeet',
    autoSave,
    onModelSelect,
    displayName: getModelDisplayName
  });

  // Progress throttle map to prevent rapid updates
  const progressThrottleRef = useRef<Map<string, { progress: number; timestamp: number }>>(new Map());
  const latestStatusByModelRef = useRef<Map<string, ModelStatus>>(new Map());
  const clearCancellingModel = (modelName: string) => {
    setCancellingModels(prev => {
      const next = new Set(prev);
      next.delete(modelName);
      return next;
    });
  };

  // Initialize and load models
  useEffect(() => {
    if (initialized || !listenersReady) return;

    const initializeModels = async () => {
      try {
        setLoading(true);
        await ParakeetAPI.init();
        // The engine reports active downloads with their latest progress.
        const modelList = await ParakeetAPI.getAvailableModels();
        const restored = modelList.map(model => ({
          ...model,
          status: latestStatusByModelRef.current.get(model.name) ?? model.status
        }));
        setModels(restored);
        setDownloadingModels(new Set(restored
          .filter(model => typeof model.status === 'object' && 'Downloading' in model.status)
          .map(model => model.name)));

        setInitialized(true);
      } catch (err) {
        console.error('Failed to initialize Parakeet:', err);
        setError(err instanceof Error ? err.message : 'Failed to load models');
        toast.error('Failed to load transcription models', {
          description: err instanceof Error ? err.message : 'Unknown error',
          duration: 5000
        });
      } finally {
        setLoading(false);
      }
    };

    initializeModels();
  }, [initialized, listenersReady, selectedModel, onModelSelect]);

  // Set up event listeners for download progress
  useEffect(() => {
    let disposed = false;
    let registeredUnlisteners: Array<() => void> = [];

    const setupListeners = async () => {
      console.log('[ParakeetModelManager] Setting up event listeners...');

      const registrations = await Promise.allSettled([
        listen<ParakeetDownloadProgressEvent>(
          'parakeet-model-download-progress',
          (event) => {
            const { modelName, progress, status } = event.payload;
            if (status === 'cancelled') {
              latestStatusByModelRef.current.set(modelName, 'Missing');
              progressThrottleRef.current.delete(modelName);
              clearCancellingModel(modelName);
              setDownloadingModels(prev => {
                const next = new Set(prev);
                next.delete(modelName);
                return next;
              });
              setModels(prevModels =>
                prevModels.map(model =>
                  model.name === modelName
                    ? { ...model, status: 'Missing' as ModelStatus }
                    : model
                )
              );
              toast.info(`${getModelDisplayName(modelName)} download cancelled`, {
                duration: 3000
              });
              return;
            }

            const modelStatus: ModelStatus = status === 'completed'
              ? 'Available'
              : { Downloading: { progress } };
            latestStatusByModelRef.current.set(modelName, modelStatus);

            const now = Date.now();
            const throttleData = progressThrottleRef.current.get(modelName);
            const shouldUpdate = !throttleData ||
              now - throttleData.timestamp > 300 ||
              Math.abs(progress - throttleData.progress) >= 5;

            if (shouldUpdate) {
              console.log(`[ParakeetModelManager] Progress update for ${modelName}: ${progress}%`);
              progressThrottleRef.current.set(modelName, { progress, timestamp: now });
              setModels(prevModels =>
                prevModels.map(model =>
                  model.name === modelName
                    ? { ...model, status: modelStatus }
                    : model
                )
              );
            }
          }
        ),
        listen<{ modelName: string }>(
          'parakeet-model-download-complete',
          (event) => {
            const { modelName } = event.payload;
            const displayName = getModelDisplayName(modelName);
            latestStatusByModelRef.current.set(modelName, 'Available');
            clearCancellingModel(modelName);

            setModels(prevModels =>
              prevModels.map(model =>
                model.name === modelName
                  ? { ...model, status: 'Available' as ModelStatus }
                  : model
              )
            );

            setDownloadingModels(prev => {
              const newSet = new Set(prev);
              newSet.delete(modelName);
              return newSet;
            });

            // Clean up throttle data
            progressThrottleRef.current.delete(modelName);

            // Keep the saved model until the user chooses the new one.
            toast.success(`${displayName} is ready`, {
              description: 'Select it to use it for transcription.',
              duration: 4000
            });
          }
        ),
        listen<{ modelName: string; error: string }>(
          'parakeet-model-download-error',
          (event) => {
            const { modelName, error } = event.payload;
            const displayInfo = getModelDisplayInfo(modelName);
            const displayName = displayInfo?.friendlyName || modelName;
            latestStatusByModelRef.current.set(modelName, { Error: error });
            clearCancellingModel(modelName);

            setModels(prevModels =>
              prevModels.map(model =>
                model.name === modelName
                  ? { ...model, status: { Error: error } as ModelStatus }
                  : model
              )
            );

            setDownloadingModels(prev => {
              const newSet = new Set(prev);
              newSet.delete(modelName);
              return newSet;
            });

            // Clean up throttle data
            progressThrottleRef.current.delete(modelName);

            toast.error(`Failed to download ${displayName}`, {
              description: error,
              duration: 6000,
              action: {
                label: 'Retry',
                onClick: () => downloadModel(modelName)
              }
            });
          }
        )
      ]);

      const unlisteners = registrations.flatMap(result =>
        result.status === 'fulfilled' ? [result.value] : []
      );
      const failedRegistration = registrations.find(
        (result): result is PromiseRejectedResult => result.status === 'rejected'
      );

      if (disposed || failedRegistration) {
        unlisteners.forEach(unlisten => unlisten());
        if (!disposed && failedRegistration) {
          const message = failedRegistration.reason instanceof Error
            ? failedRegistration.reason.message
            : typeof failedRegistration.reason === 'string'
              ? failedRegistration.reason
              : 'Failed to listen for Parakeet model updates';
          setError(message);
          setLoading(false);
        }
        return;
      }

      registeredUnlisteners = unlisteners;
      setListenersReady(true);
    };

    setupListeners();

    return () => {
      console.log('[ParakeetModelManager] Cleaning up event listeners...');
      disposed = true;
      registeredUnlisteners.forEach(unlisten => unlisten());
    };
  }, []); // Empty dependency array - listeners use refs for stable callbacks

  const cancelDownload = async (modelName: string) => {
    const displayInfo = getModelDisplayInfo(modelName);
    const displayName = displayInfo?.friendlyName || modelName;

    setCancellingModels(prev => new Set([...prev, modelName]));
    try {
      const outcome = await ParakeetAPI.cancelDownload(modelName);
      if (outcome === 'pending') {
        toast.info(`Cancelling ${displayName}...`, {
          description: 'The download is still shutting down. Retry will be available when cleanup completes.',
          duration: 4000
        });
      }
    } catch (err) {
      clearCancellingModel(modelName);
      console.error('Failed to cancel download:', err);
      toast.error('Failed to cancel download', {
        description: err instanceof Error ? err.message : 'Unknown error',
        duration: 4000
      });
    }
  };

  const downloadModel = async (modelName: string) => {
    if (downloadingModels.has(modelName) || cancellingModels.has(modelName)) return;
    clearCancellingModel(modelName);

    try {
      setDownloadingModels(prev => new Set([...prev, modelName]));

      setModels(prevModels =>
        prevModels.map(model =>
          model.name === modelName
            ? { ...model, status: { Downloading: { progress: 0 } } as ModelStatus }
            : model
        )
      );

      await ParakeetAPI.downloadModel(modelName);
    } catch (err) {
      console.error('Download failed:', err);
      setDownloadingModels(prev => {
        const newSet = new Set(prev);
        newSet.delete(modelName);
        return newSet;
      });

      const message = errorMessage(err) || 'Download failed';
      setModels(prev =>
        prev.map(model =>
          model.name === modelName ? { ...model, status: { Error: message } } : model
        )
      );
    }
  };

  const deletionPendingRef = useRef(false);

  const deleteModel = async (model: ParakeetModelInfo) => {
    const modelName = model.name;
    const displayName = getModelDisplayName(modelName);
    // A ready selected model must be replaced before it is removed. Damaged or
    // partial files of the selected model can still be deleted: that selection
    // was already unusable, and SelectedModelNote says so.
    if (selectedModel === modelName && model.status === 'Available') {
      toast.error('Select another model before deleting this one');
      return;
    }
    if (deletionPendingRef.current) return;
    deletionPendingRef.current = true;
    try {
      const confirmed = await confirm(
        `Delete transcription model ${displayName}? This will remove it from disk.`,
        { title: 'Delete transcription model?', kind: 'warning', okLabel: 'Delete', cancelLabel: 'Cancel' }
      );
      if (!confirmed) {
        return;
      }
      await ParakeetAPI.deleteCorruptedModel(modelName);

      // Refresh models list
      const modelList = await ParakeetAPI.getAvailableModels();
      setModels(modelList);

      toast.success(`${displayName} deleted`, {
        description: 'Model removed to free up space',
        duration: 3000
      });
    } catch (err) {
      console.error('Failed to delete model:', err);
      toast.error(`Failed to delete ${displayName}`, {
        description: err instanceof Error ? err.message : 'Delete failed',
        duration: 4000
      });
    } finally {
      deletionPendingRef.current = false;
    }
  };

  if (loading) {
    return <div className={className}><ModelListSkeleton rows={2} /></div>;
  }

  if (error) {
    return <div className={className}><ModelListError message={error} /></div>;
  }

  // Recommended model first, then the others in engine order.
  const orderedModels = [
    ...models.filter(model => model.name === RECOMMENDED_MODEL),
    ...models.filter(model => model.name !== RECOMMENDED_MODEL)
  ];

  return (
    <div className={`space-y-3 ${className}`}>
      {orderedModels.map(model => {
        const displayInfo = getModelDisplayInfo(model.name);
        return (
          <TranscriptionModelCard
            key={model.name}
            displayName={getModelDisplayName(model.name)}
            icon={displayInfo?.icon || '📦'}
            description={displayInfo?.tagline || model.description}
            details={[formatFileSize(model.size_mb), `${model.quantization} weights`]}
            recommended={model.name === RECOMMENDED_MODEL}
            selected={selectedModel === model.name}
            saving={savingModel === model.name}
            selectionLocked={savingModel !== null}
            status={modelCardStatus(model.status, {
              cancelling: cancellingModels.has(model.name),
              detail: progress => model.size_mb
                ? `${formatFileSize(Math.round(model.size_mb * progress / 100))} / ${formatFileSize(model.size_mb)}`
                : undefined
            })}
            onSelect={() => selectModel(model.name)}
            onDownload={() => downloadModel(model.name)}
            onCancel={() => cancelDownload(model.name)}
            onDelete={() => deleteModel(model)}
          />
        );
      })}

      <SelectedModelNote
        displayName={selectedModel ? getModelDisplayName(selectedModel) : undefined}
        ready={models.some(model => model.name === selectedModel && model.status === 'Available')}
      />
    </div>
  );
}
