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
  has_partial_download: boolean;
  has_orphaned_encoder: boolean;
  size_mb: number;
  decoder_size_mb: number;
  encoder_size_mb: number;
  description: string;
  recommended: boolean;
  status: FunAsrModelStatus;
}

export const FUNASR_SUPPORTED_LANGUAGES = new Set(['auto', 'en', 'zh', 'ja']);

// NeMo-Speech.cpp v0.3.0, src/speechlm/salm_policy.cpp::language_names.
export const NEMOTRON_SUPPORTED_LANGUAGES = new Set(['auto', 'en', 'ar', 'zh', 'fr', 'de', 'hi', 'it', 'ja', 'es', 'ko', 'pt', 'ru', 'th', 'hy', 'nn', 'nb', 'no', 'bg', 'hr', 'cs', 'da', 'nl', 'et', 'fi', 'el', 'he', 'hu', 'id', 'lv', 'lt', 'mt', 'ta', 'bn', 'pl', 'ro', 'sk', 'sl', 'sv', 'tr', 'tl', 'fil', 'uk', 'vi']);

/** The languages a transcription provider accepts, from a full language list. */
export function languagesForProvider<T extends { code: string; name: string }>(provider: string | undefined, languages: T[]): T[] {
  if (provider === 'nemotron') return languages
    .filter(language => NEMOTRON_SUPPORTED_LANGUAGES.has(language.code))
    .map(language => language.code === 'auto' ? { ...language, name: 'Auto (No language hint)' } : language);
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
