import { useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';

interface ModelSelectionOptions {
  provider: 'localWhisper' | 'parakeet' | 'funasr' | 'nemotron';
  autoSave: boolean;
  onModelSelect?: (modelName: string) => void;
  displayName: (modelName: string) => string;
}

/**
 * Saves a local model selection before reporting it, so a failed save never
 * changes the displayed model or closes Settings. One save runs at a time.
 */
export function useModelSelection({ provider, autoSave, onModelSelect, displayName }: ModelSelectionOptions) {
  const [savingModel, setSavingModel] = useState<string | null>(null);
  const savingRef = useRef(false);

  const selectModel = async (modelName: string) => {
    if (savingRef.current) return;
    savingRef.current = true;
    setSavingModel(modelName);
    try {
      if (autoSave) {
        await invoke('api_save_transcript_config', { provider, model: modelName, apiKey: null });
      }
      onModelSelect?.(modelName);
      toast.success(`Switched to ${displayName(modelName)}`, { duration: 3000 });
    } catch (error) {
      toast.error('Could not save the transcription model', {
        description: error instanceof Error ? error.message : String(error),
      });
    } finally {
      savingRef.current = false;
      setSavingModel(null);
    }
  };

  return { savingModel, selectModel };
}
