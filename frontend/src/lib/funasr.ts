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
  encoder_available: boolean;
  size_mb: number;
  decoder_size_mb: number;
  encoder_size_mb: number;
  description: string;
  recommended: boolean;
  status: FunAsrModelStatus;
}

export const FUNASR_SUPPORTED_LANGUAGES = new Set(['auto', 'en', 'zh', 'ja']);

/** The languages a transcription provider accepts, from a full language list. */
export function languagesForProvider<T extends { code: string }>(provider: string | undefined, languages: T[]): T[] {
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

  static cancelDownload(modelName: string): Promise<void> {
    return invoke('funasr_cancel_download', { modelName });
  }

  static deleteModel(modelName: string): Promise<string> {
    return invoke('funasr_delete_model', { modelName });
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
