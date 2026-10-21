import { useState, useCallback, useRef, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';

export interface RawModelInfo {
  name: string;
  size_mb: number;
  status: 'Available' | 'Missing' | { Downloading: { progress: number } } | { Error: string };
}

export interface ModelOption {
  provider: 'whisper' | 'parakeet' | 'funasr';
  name: string;
  displayName: string;
  size_mb: number;
}

interface TranscriptModelConfig {
  provider?: string;
  model?: string;
}

/**
 * Custom hook for fetching and managing transcription models (Whisper, Parakeet and FUN-ASR).
 *
 * This hook centralizes the model fetching logic that was previously duplicated
 * in ImportAudioDialog and RetranscribeDialog components.
 *
 * @param transcriptModelConfig - User's saved model configuration from context
 * @returns Object containing available models, selected model key, loading state, and fetch function
 */
export function useTranscriptionModels(transcriptModelConfig: TranscriptModelConfig | undefined) {
  const [availableModels, setAvailableModels] = useState<ModelOption[]>([]);
  const [selectedModelKey, setSelectedModelKey] = useState<string>('');
  const [loadingModels, setLoadingModels] = useState(false);
  // Track whether the user has manually changed the model selection
  const userSelectedRef = useRef(false);

  // Wrap setSelectedModelKey to track user-initiated changes
  const setSelectedModelKeyWithTracking = useCallback((key: string) => {
    userSelectedRef.current = true;
    setSelectedModelKey(key);
  }, []);

  const [modelLoadError, setModelLoadError] = useState<string | null>(null);
  const requestIdRef = useRef(0);

  const fetchModels = useCallback(async () => {
    const requestId = ++requestIdRef.current;
    setLoadingModels(true);
    setModelLoadError(null);
    const providers = [
      { provider: 'whisper' as const, label: 'Whisper', init: 'whisper_init', list: 'whisper_get_available_models' },
      { provider: 'parakeet' as const, label: 'Parakeet', init: 'parakeet_init', list: 'parakeet_get_available_models' },
      { provider: 'funasr' as const, label: 'FUN-ASR', init: 'funasr_init', list: 'funasr_get_available_models' },
    ];
    const results = await Promise.all(providers.map(async ({ provider, label, init, list }) => {
      try {
        // Startup initializes engines in the background. Await initialization
        // here too, so a dialog opened at startup has a complete model list.
        await invoke(init);
        const models = await invoke<Array<RawModelInfo & { display_name?: string }>>(list);
        return { models: models.filter(model => model.status === 'Available').map(model => ({
          provider, name: model.name, size_mb: model.size_mb,
          displayName: `${label}: ${model.display_name ?? model.name}`,
        })), error: null };
      } catch (error) {
        return { models: [], error: `Could not load ${label} models: ${String(error)}` };
      }
    }));
    if (requestId !== requestIdRef.current) return;
    setAvailableModels(results.flatMap(result => result.models));
    setModelLoadError(results.flatMap(result => result.error ? [result.error] : []).join(' ') || null);
    setLoadingModels(false);
  }, []);

  useEffect(() => {
    const configuredProvider = transcriptModelConfig?.provider === 'localWhisper'
      ? 'whisper' : transcriptModelConfig?.provider;
    setSelectedModelKey(current => {
      if (userSelectedRef.current) {
        return availableModels.some(model => `${model.provider}:${model.name}` === current) ? current : '';
      }
      const match = availableModels.find(model => model.provider === configuredProvider && model.name === transcriptModelConfig?.model);
      return match ? `${match.provider}:${match.name}` : '';
    });
  }, [availableModels, transcriptModelConfig?.provider, transcriptModelConfig?.model]);

  const modelNotice = modelLoadError || (!loadingModels && !selectedModelKey
    ? transcriptModelConfig?.provider && transcriptModelConfig?.model
      ? `The saved model (${transcriptModelConfig.model}) is not available. Download it in Settings or select another model.`
      : 'Select a transcription model. No model will be selected automatically.'
    : null);

  // Reset user selection tracking (call when dialog opens fresh)
  const resetSelection = useCallback(() => {
    userSelectedRef.current = false;
    setSelectedModelKey('');
  }, []);

  return {
    availableModels,
    selectedModelKey,
    setSelectedModelKey: setSelectedModelKeyWithTracking,
    loadingModels,
    modelNotice,
    fetchModels,
    resetSelection,
  };
}
