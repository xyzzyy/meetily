import { invoke } from '@tauri-apps/api/core';

export type DownloadStatus = 'downloading' | 'completed' | 'error' | 'cancelled';
export interface DownloadEvent {
  modelName?: string;
  model?: string;
  progress?: number;
  percent?: number;
  downloaded_mb?: number;
  total_mb?: number;
  downloaded_bytes?: number;
  total_bytes?: number;
  speed_mbps?: number;
  status?: string;
  error?: string | null;
  endpoint?: string | null;
}
export interface DownloadProvider {
  id: string;
  label: string;
  list: string;
  statusFormat?: 'models' | 'installed' | 'worker';
  progress: string;
  complete?: string;
  error?: string;
  cancelled?: string;
  finished?: string;
  fixedModel?: string;
}

// Backend formats belong here. Notice state and lifetime are shared by every provider.
export const downloadProviders: DownloadProvider[] = [
  { id: 'parakeet', label: 'Transcription Model (Parakeet)', list: 'parakeet_get_available_models',
    progress: 'parakeet-model-download-progress', complete: 'parakeet-model-download-complete', error: 'parakeet-model-download-error' },
  { id: 'whisper', label: 'Transcription Model (Whisper)', list: 'whisper_get_available_models',
    progress: 'model-download-progress', complete: 'model-download-complete', error: 'model-download-error', cancelled: 'model-download-cancelled' },
  { id: 'funasr', label: 'Transcription Model (FUN-ASR)', list: 'funasr_get_available_models',
    progress: 'funasr-model-download-progress', complete: 'funasr-model-download-complete', error: 'funasr-model-download-error' },
  { id: 'nemotron', label: 'Transcription Model (Nemotron)', list: 'nemotron_get_available_models',
    progress: 'nemotron-download-progress', finished: 'nemotron-download-finished' },
  { id: 'builtin', label: 'Summary Model', list: 'builtin_ai_list_models', progress: 'builtin-ai-download-progress' },
  { id: 'ollama', label: 'Ollama Model', list: 'get_ollama_download_status', statusFormat: 'worker',
    progress: 'ollama-model-download-progress', complete: 'ollama-model-download-complete', error: 'ollama-model-download-error' },
  { id: 'diarization', label: 'Speaker Models', list: 'get_diarization_model_status', statusFormat: 'installed',
    progress: 'diarization-model-download-progress', finished: 'diarization-model-download-finished', fixedModel: 'speaker-models' },
];

export function eventStatus(event: DownloadEvent, fallback: DownloadStatus): DownloadStatus {
  if (event.status === 'cancelled' || event.error?.toLowerCase().includes('cancelled')) return 'cancelled';
  if (event.status === 'error' || event.error) return 'error';
  if (event.status === 'completed' || event.status === 'complete') return 'completed';
  // A file or phase can reach 100% before validation or installation ends.
  return fallback;
}

type Model = { name: string; status: string | { type?: string; Error?: string; Corrupted?: unknown; Downloading?: unknown } };
export async function checkDownloads(provider: DownloadProvider, models: { modelName: string; endpoint?: string | null }[]): Promise<Map<string, DownloadStatus>> {
  const result = new Map<string, DownloadStatus>();
  if (provider.statusFormat === 'worker') {
    await Promise.all(models.map(async model => {
      const status = await invoke<DownloadStatus>(provider.list, { modelName: model.modelName, endpoint: model.endpoint });
      result.set(model.modelName, status);
    }));
  } else if (provider.statusFormat === 'installed') {
    const status = await invoke<{ installed: boolean }>(provider.list);
    if (status.installed) models.forEach(model => result.set(model.modelName, 'completed'));
  } else {
    const available = await invoke<Model[]>(provider.list);
    for (const model of models) {
      const status = available.find(item => item.name === model.modelName)?.status;
      if (status === 'Available' || (typeof status === 'object' && status?.type === 'available')) {
        result.set(model.modelName, 'completed');
      } else if (status === 'Missing' || (typeof status === 'object' && status &&
        ('Error' in status || 'Corrupted' in status || ['not_downloaded', 'corrupted', 'error'].includes(status.type ?? '')))) {
        result.set(model.modelName, 'error');
      }
    }
  }
  return result;
}
