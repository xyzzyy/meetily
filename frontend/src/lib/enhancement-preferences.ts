import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { languagesForProvider } from '@/lib/funasr';
import { LANGUAGES } from '@/constants/languages';

const KEY = 'enhancementPreferences';
export interface EnhancementPreferences {
  autoEnhance: boolean;
  provider: 'whisper' | 'parakeet' | 'funasr' | 'nemotron' | '';
  model: string;
}
const defaults: EnhancementPreferences = { autoEnhance: false, provider: '', model: '' };

export function readEnhancementPreferences(): EnhancementPreferences {
  try {
    const value = JSON.parse(window.localStorage.getItem(KEY) || 'null');
    if (!value || !['', 'whisper', 'parakeet', 'funasr', 'nemotron'].includes(value.provider)
      || typeof value.model !== 'string') return { ...defaults };
    return { provider: value.provider, model: value.model, autoEnhance: value.autoEnhance === true };
  } catch { return { ...defaults }; }
}

export function writeEnhancementPreferences(value: EnhancementPreferences): void {
  window.localStorage.setItem(KEY, JSON.stringify(value));
}

const pendingEnhancements = new Map<string, Promise<void>>();

export async function waitForAutoEnhancement(meetingId: string): Promise<void> {
  await pendingEnhancements.get(meetingId);
}

/** Start only after the meeting and its audio have been saved. */
export async function startAutoEnhancement(meetingId: string, meetingFolderPath: string | null): Promise<boolean> {
  const settings = readEnhancementPreferences();
  if (!settings.autoEnhance) return false;
  if (!settings.provider || !settings.model) throw new Error('Select a default enhancement model in Transcription settings.');
  if (!meetingFolderPath) throw new Error('The recording has no saved audio folder.');
  let finish!: () => void;
  const pending = new Promise<void>(resolve => { finish = resolve; });
  pendingEnhancements.set(meetingId, pending);
  const unlisteners: (() => void)[] = [];
  const complete = () => {
    finish();
    pendingEnhancements.delete(meetingId);
    unlisteners.forEach(unlisten => unlisten());
  };
  try {
    for (const name of ['retranscription-complete', 'retranscription-error']) {
      unlisteners.push(await listen<{ meeting_id: string }>(name, event => {
        if (event.payload.meeting_id === meetingId) complete();
      }));
    }
    await invoke(`${settings.provider}_init`);
    const models = await invoke<{ name: string; status: unknown }[]>(`${settings.provider}_get_available_models`);
    if (!models.some(model => model.name === settings.model && model.status === 'Available')) {
      complete();
      return false;
    }
    const diarization = await invoke<{ enabled: boolean; postCallRefinementEnabled: boolean }>('get_diarization_settings');
    const language = window.localStorage.getItem('primaryLanguage') || 'auto';
    const supportedLanguage = languagesForProvider(settings.provider, LANGUAGES).some(item => item.code === language);
    let assignSpeakers = diarization.enabled && diarization.postCallRefinementEnabled;
    if (assignSpeakers && settings.provider !== 'nemotron') {
      const status = await invoke<{ refinementSupported: boolean }>('get_diarization_model_status');
      assignSpeakers = status.refinementSupported;
    }
    await invoke('start_retranscription_command', {
      meetingId, meetingFolderPath, provider: settings.provider, model: settings.model,
      language: settings.provider === 'parakeet' || language === 'auto' || !supportedLanguage ? null : language,
      assignSpeakers,
    });
    return true;
  } catch (error) {
    complete();
    throw error;
  }
}
