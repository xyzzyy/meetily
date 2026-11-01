import { invoke } from '@tauri-apps/api/core';

export type FunAsrModelStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string };

export interface FunAsrModelInfo {
  name: string;
  display_name: string;
  path: string;
  encoder_is_shared?: boolean;
  decoder_is_shared?: boolean;
  encoder_available: boolean;
  has_partial_download: boolean;
  has_orphaned_encoder: boolean;
  size_mb: number;
  decoder_size_mb: number;
  encoder_size_mb: number;
  description: string;
  recommended: boolean;
  status: FunAsrModelStatus;
}

/** Missing can mean absent or damaged files; a false shared flag does not prove a Branch file exists. */
export function funAsrStorageNote(model: Pick<FunAsrModelInfo, 'status' | 'encoder_is_shared' | 'decoder_is_shared'>): string | undefined {
  const encoder = model.encoder_is_shared;
  const decoder = model.decoder_is_shared;
  if (encoder === undefined || decoder === undefined) return undefined;
  if (model.status === 'Available') {
    if (encoder && decoder) return 'Shared from official Meetily. Read-only in Branch.';
    if (encoder) return 'Decoder saved in Meetily Branch. Encoder shared from official Meetily.';
    if (decoder) return 'Encoder saved in Meetily Branch. Decoder shared from official Meetily.';
    return 'Saved in Meetily Branch.';
  }
  const destination = 'Downloads are saved in Meetily Branch.';
  if (!encoder && !decoder) return destination;
  const source = encoder && decoder ? 'Encoder and decoder are shared from official Meetily.'
    : encoder ? 'Encoder is shared from official Meetily.' : 'Decoder is shared from official Meetily.';
  return `${source} Shared files are read-only in Branch. ${destination}`;
}

export const FUNASR_SUPPORTED_LANGUAGES = new Set(['auto', 'en', 'zh', 'ja']);

/** The languages a transcription provider accepts, from a full language list. */
export function languagesForProvider<T extends { code: string }>(provider: string | undefined, languages: T[]): T[] {
  if (provider === 'nemotron') return languages.filter(language => language.code === 'auto');
  return provider === 'funasr'
    ? languages.filter((language) => FUNASR_SUPPORTED_LANGUAGES.has(language.code))
    : languages;
}

export class FunAsrAPI {
  static init(): Promise<void> {
    return invoke('funasr_init');
  }

  static getAvailableModels(): Promise<FunAsrModelInfo[]> {
    return invoke('funasr_get_available_models');
  }

  static downloadModel(modelName: string): Promise<void> {
    return invoke('funasr_download_model', { modelName });
  }

  /** True: cancellation requested. False: no active download. */
  static cancelDownload(modelName: string): Promise<boolean> {
    return invoke('funasr_cancel_download', { modelName });
  }

  static deleteModel(modelName: string, partialOnly = false): Promise<string> {
    return invoke('funasr_delete_model', { modelName, ...(partialOnly ? { partialOnly: true } : {}) });
  }

  static loadModel(modelName: string): Promise<void> {
    return invoke('funasr_load_model', { modelName });
  }
}

export function funAsrProgress(status: FunAsrModelStatus): number | null {
  if (typeof status === 'object' && 'Downloading' in status) {
    return status.Downloading.progress;
  }
  return null;
}
