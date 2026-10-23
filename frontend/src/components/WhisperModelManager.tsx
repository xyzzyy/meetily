import React, { useState, useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { toast } from 'sonner';
import {
  ModelInfo,
  ModelStatus,
  getModelIcon,
  formatFileSize,
  getModelPerformanceBadge,
  isQuantizedModel,
  getModelTagline,
  WhisperAPI
} from '../lib/whisper';
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from '@/components/ui/accordion';
import { useModelSelection } from '@/hooks/useModelSelection';
import {
  ModelListError,
  ModelListSkeleton,
  SelectedModelNote,
  TranscriptionModelCard,
  errorMessage,
  modelCardStatus
} from './TranscriptionModelCard';

interface ModelManagerProps {
  selectedModel?: string;
  onModelSelect?: (modelName: string) => void;
  className?: string;
  autoSave?: boolean;
}

export function ModelManager({
  selectedModel,
  onModelSelect,
  className = '',
  autoSave = false
}: ModelManagerProps) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [initialized, setInitialized] = useState(false);
  const [downloadingModels, setDownloadingModels] = useState<Set<string>>(new Set());
  const [cancellingModels, setCancellingModels] = useState<Set<string>>(new Set());
  const { savingModel, selectModel } = useModelSelection({
    provider: 'localWhisper',
    autoSave,
    onModelSelect,
    displayName: getDisplayName
  });

  // Progress throttle map to prevent rapid updates
  const progressThrottleRef = useRef<Map<string, { progress: number; timestamp: number }>>(new Map());
  const cancellationReconciliationModelsRef = useRef<Set<string>>(new Set());
  const cancellationReconcileTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  const clearCancellationReconciliation = (modelName: string) => {
    cancellationReconciliationModelsRef.current.delete(modelName);
    const timer = cancellationReconcileTimersRef.current.get(modelName);
    if (timer !== undefined) {
      clearTimeout(timer);
      cancellationReconcileTimersRef.current.delete(modelName);
    }
    setCancellingModels(prev => {
      const next = new Set(prev);
      next.delete(modelName);
      return next;
    });
  };

  const reconcileCancellation = (modelName: string) => {
    if (cancellationReconciliationModelsRef.current.has(modelName)) return;

    cancellationReconciliationModelsRef.current.add(modelName);
    setCancellingModels(prev => new Set([...prev, modelName]));

    const scheduleNextCheck = () => {
      if (!cancellationReconciliationModelsRef.current.has(modelName)) return;
      const timer = setTimeout(() => {
        cancellationReconcileTimersRef.current.delete(modelName);
        void reconcile();
      }, 1000);
      cancellationReconcileTimersRef.current.set(modelName, timer);
    };

    const reconcile = async () => {
      try {
        const modelList = await WhisperAPI.getAvailableModels();
        if (!cancellationReconciliationModelsRef.current.has(modelName)) return;

        const model = modelList.find(candidate => candidate.name === modelName);

        const isStillDownloading = typeof model?.status === 'object' && 'Downloading' in model.status;
        if (model && !isStillDownloading) {
          clearCancellationReconciliation(modelName);
          setDownloadingModels(prev => {
            const next = new Set(prev);
            next.delete(modelName);
            return next;
          });
          setModels(modelList);
          progressThrottleRef.current.delete(modelName);
          toast.info(
            model.status === 'Available'
              ? `${getDisplayName(modelName)} download completed before cancellation`
              : `${getDisplayName(modelName)} download cancelled`,
            { duration: 3000 }
          );
          return;
        }
      } catch (err) {
        console.warn('Failed to reconcile pending download cancellation:', err);
      }

      scheduleNextCheck();
    };

    void reconcile();
  };

  // Initialize models
  useEffect(() => {
    if (initialized) return;

    const initializeModels = async () => {
      try {
        setLoading(true);
        await WhisperAPI.init();
        // The engine reports active downloads with their latest progress.
        const modelList = await WhisperAPI.getAvailableModels();
        setModels(modelList);
        setDownloadingModels(new Set(modelList
          .filter(model => typeof model.status === 'object' && 'Downloading' in model.status)
          .map(model => model.name)));
        setInitialized(true);
      } catch (err) {
        console.error('Failed to initialize Whisper:', err);
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
  }, [initialized, selectedModel, onModelSelect]);

  // Set up event listeners for download progress
  useEffect(() => {
    let unlistenProgress: (() => void) | null = null;
    let unlistenComplete: (() => void) | null = null;
    let unlistenError: (() => void) | null = null;

    const setupListeners = async () => {
      console.log('[ModelManager] Setting up event listeners...');

      // Download progress with throttling
      unlistenProgress = await listen<{ modelName: string; progress: number }>(
        'model-download-progress',
        (event) => {
          const { modelName, progress } = event.payload;
          const now = Date.now();
          const throttleData = progressThrottleRef.current.get(modelName);

          // Throttle: only update if 300ms passed OR progress jumped by 5%+
          const shouldUpdate = !throttleData ||
            now - throttleData.timestamp > 300 ||
            Math.abs(progress - throttleData.progress) >= 5;

          if (shouldUpdate) {
            console.log(`[ModelManager] Progress update for ${modelName}: ${progress}%`);
            progressThrottleRef.current.set(modelName, { progress, timestamp: now });

            setModels(prevModels =>
              prevModels.map(model =>
                model.name === modelName
                  ? { ...model, status: { Downloading: { progress } } as ModelStatus }
                  : model
              )
            );
          }
        }
      );

      // Download complete
      unlistenComplete = await listen<{ modelName: string }>(
        'model-download-complete',
        (event) => {
          const { modelName } = event.payload;
          const displayName = getDisplayName(modelName);

          clearCancellationReconciliation(modelName);

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
      );

      // Download error
      unlistenError = await listen<{ modelName: string; error: string }>(
        'model-download-error',
        (event) => {
          const { modelName, error } = event.payload;
          const displayName = getDisplayName(modelName);

          clearCancellationReconciliation(modelName);

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
      );
    };

    setupListeners();

    return () => {
      console.log('[ModelManager] Cleaning up event listeners...');
      if (unlistenProgress) unlistenProgress();
      if (unlistenComplete) unlistenComplete();
      if (unlistenError) unlistenError();
      for (const timer of cancellationReconcileTimersRef.current.values()) {
        clearTimeout(timer);
      }
      cancellationReconcileTimersRef.current.clear();
      cancellationReconciliationModelsRef.current.clear();
    };
  }, []); // Empty dependency array - listeners use refs for stable callbacks

  const cancelDownload = async (modelName: string) => {
    const displayName = getDisplayName(modelName);

    try {
      const outcome = await WhisperAPI.cancelDownload(modelName);
      if (outcome === 'pending') {
        reconcileCancellation(modelName);
        toast.info(`Cancelling ${displayName}...`, {
          description: 'The download is still shutting down. Retry will be available when cleanup completes.',
          duration: 4000
        });
        return;
      }

      // A worker can finish between the user's click and the cancellation command
      // acquiring its owner. Reconcile instead of overwriting a valid Available state.
      reconcileCancellation(modelName);
    } catch (err) {
      console.error('Failed to cancel download:', err);
      toast.error('Failed to cancel download', {
        description: err instanceof Error ? err.message : 'Unknown error',
        duration: 4000
      });
    }
  };

  const downloadModel = async (modelName: string) => {
    if (downloadingModels.has(modelName)) return;

    clearCancellationReconciliation(modelName);

    try {
      setDownloadingModels(prev => new Set([...prev, modelName]));

      setModels(prevModels =>
        prevModels.map(model =>
          model.name === modelName
            ? { ...model, status: { Downloading: { progress: 0 } } as ModelStatus }
            : model
        )
      );

      await WhisperAPI.downloadModel(modelName);
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

  const deleteModel = async (model: ModelInfo) => {
    const modelName = model.name;
    const displayName = getDisplayName(modelName);
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
      await WhisperAPI.deleteCorruptedModel(modelName);

      // Refresh models list
      const modelList = await WhisperAPI.getAvailableModels();
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
    return <div className={className}><ModelListSkeleton rows={3} /></div>;
  }

  if (error) {
    return <div className={className}><ModelListError message={error} /></div>;
  }

  const basicModels = models.filter(m => BASIC_MODEL_NAMES.includes(m.name))
    .sort((a, b) => BASIC_MODEL_NAMES.indexOf(a.name) - BASIC_MODEL_NAMES.indexOf(b.name));
  const advancedModels = models.filter(m => !BASIC_MODEL_NAMES.includes(m.name));

  const renderCard = (model: ModelInfo) => (
    <TranscriptionModelCard
      key={model.name}
      displayName={getDisplayName(model.name)}
      icon={getModelIcon(model.accuracy)}
      description={getModelTagline(model.name, model.speed, model.accuracy)}
      details={[formatFileSize(model.size_mb), `${model.accuracy} accuracy`, `${model.speed} processing`]}
      badges={isQuantizedModel(model.name) && <QuantizationBadge modelName={model.name} />}
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

  return (
    <div className={`space-y-3 ${className}`}>
      <div className="space-y-3">
        {basicModels.map(renderCard)}
      </div>

      {advancedModels.length > 0 && (
        <Accordion type="single" collapsible className="w-full">
          <AccordionItem value="advanced-models">
            <AccordionTrigger>
              <span className='text-lg'>Advanced Models</span>
            </AccordionTrigger>
            <AccordionContent>
              <div className="space-y-3 pt-4">
                {advancedModels.map(renderCard)}
              </div>
            </AccordionContent>
          </AccordionItem>
        </Accordion>
      )}

      <SelectedModelNote
        displayName={selectedModel ? getDisplayName(selectedModel) : undefined}
        ready={models.some(model => model.name === selectedModel && model.status === 'Available')}
      />
    </div>
  );
}

const BASIC_MODEL_NAMES = ["small", "medium-q5_0", "large-v3-q5_0", "large-v3-turbo", "large-v3"];

function getDisplayName(modelName: string): string {
  const modelNameMapping: { [key: string]: string } = {
    "small": "Small",
    "medium-q5_0": "Medium",
    "large-v3-q5_0": "Large V3 Compressed",
    "large-v3-turbo": "Large V3 Turbo",
    "large-v3": "Large V3"
  };
  return modelNameMapping[modelName] || `Whisper ${modelName}`;
}

function QuantizationBadge({ modelName }: { modelName: string }) {
  const badge = getModelPerformanceBadge(modelName);
  const color = badge.color === 'green'
    ? 'bg-green-100 dark:bg-green-500/15 text-green-700 dark:text-green-300'
    : badge.color === 'orange'
      ? 'bg-orange-100 dark:bg-orange-500/15 text-orange-700 dark:text-orange-300'
      : 'bg-gray-100 dark:bg-ink-800 text-gray-700 dark:text-ink-200';
  return <span className={`px-2 py-0.5 rounded-full text-xs ${color}`}>{badge.label}</span>;
}
