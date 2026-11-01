'use client';

import React, { createContext, useContext, useState, useEffect, useCallback, useMemo, ReactNode, useRef } from 'react';
import { TranscriptModelProps } from '@/components/TranscriptSettings';
import { SelectedDevices } from '@/components/DeviceSelection';
import { configService, ModelConfig } from '@/services/configService';
import { invoke } from '@tauri-apps/api/core';
import { NEMOTRON_SUPPORTED_LANGUAGES, FUNASR_SUPPORTED_LANGUAGES } from '@/lib/funasr';
import { toast } from 'sonner';
import { BetaFeatures, BetaFeatureKey, loadBetaFeatures, saveBetaFeatures } from '@/types/betaFeatures';
import { DEFAULT_DIARIZATION_SETTINGS, type DiarizationSettings } from '@/types/diarization';

export interface OllamaModel {
  name: string;
  id: string;
  size: string;
  modified: string;
}

export interface StorageLocations {
  database: string;
  models: string;
  recordings: string;
}

export interface NotificationSettings {
  recording_notifications: boolean;
  time_based_reminders: boolean;
  meeting_reminders: boolean;
  respect_do_not_disturb: boolean;
  notification_sound: boolean;
  system_permission_granted: boolean;
  consent_given: boolean;
  manual_dnd_mode: boolean;
  notification_preferences: {
    show_recording_started: boolean;
    show_recording_stopped: boolean;
    show_recording_paused: boolean;
    show_recording_resumed: boolean;
    show_transcription_complete: boolean;
    show_meeting_reminders: boolean;
    show_system_errors: boolean;
    meeting_reminder_minutes: number[];
  };
}

interface ConfigContextType {
  // Model configuration
  modelConfig: ModelConfig;
  setModelConfig: (config: ModelConfig | ((prev: ModelConfig) => ModelConfig)) => void;
  isModelConfigLoading: boolean;
  isModelSettingsBusy: boolean;
  runModelSettingsOperation: (operation: () => Promise<void>) => Promise<boolean>;

  // Transcript model configuration
  transcriptModelConfig: TranscriptModelProps;
  setTranscriptModelConfig: (config: TranscriptModelProps | ((prev: TranscriptModelProps) => TranscriptModelProps)) => void;

  // Device configuration
  selectedDevices: SelectedDevices;
  setSelectedDevices: (devices: SelectedDevices) => void;

  // Language preference
  selectedLanguage: string;
  setSelectedLanguage: (lang: string) => void;

  // UI preferences
  showConfidenceIndicator: boolean;
  toggleConfidenceIndicator: (checked: boolean) => void;

  // Beta features
  betaFeatures: BetaFeatures;
  toggleBetaFeature: (featureKey: BetaFeatureKey, enabled: boolean) => void;

  // Speaker diarization settings
  diarizationSettings: DiarizationSettings;
  isLoadingDiarizationSettings: boolean;
  updateDiarizationSettings: (settings: DiarizationSettings) => Promise<void>;

  // Ollama models
  models: OllamaModel[];
  modelOptions: Record<ModelConfig['provider'], string[]>;
  error: string;

  // Summary configuration
  isAutoSummary: boolean;
  toggleIsAutoSummary: (checked: boolean) => void;

  // Provider-specific API keys
  providerApiKeys: {
    claude: string | null;
    groq: string | null;
    openai: string | null;
    openrouter: string | null;
  };
  updateProviderApiKey: (provider: string, apiKey: string | null) => void;
  getProviderApiKeyChange: (provider: string) => { version: number; value: string | null };

  // Preference settings (lazy loaded)
  notificationSettings: NotificationSettings | null;
  storageLocations: StorageLocations | null;
  isLoadingPreferences: boolean;
  loadPreferences: () => Promise<void>;
  updateNotificationSettings: (settings: NotificationSettings) => Promise<void>;
}

const ConfigContext = createContext<ConfigContextType | undefined>(undefined);


export function ConfigProvider({ children }: { children: ReactNode }) {
  // Model configuration state
  const [modelConfig, setModelConfig] = useState<ModelConfig>({
    provider: 'ollama',
    model: 'llama3.2:latest',
    whisperModel: 'large-v3',
    ollamaEndpoint: null
  });
  const [isModelConfigLoading, setIsModelConfigLoading] = useState(true);
  const [isModelSettingsBusy, setIsModelSettingsBusy] = useState(false);
  const modelSettingsBusyRef = useRef(false);


  // Transcript model configuration state
  const [transcriptModelConfig, setTranscriptModelConfig] = useState<TranscriptModelProps>({
    provider: '',
    model: '',
    apiKey: null
  });

  // Provider-specific API keys (loaded once at startup)
  // Note: Gemini omitted for now - add when UI support is added
  const [providerApiKeys, setProviderApiKeys] = useState<{
    claude: string | null;
    groq: string | null;
    openai: string | null;
    openrouter: string | null;
  }>({
    claude: null,
    groq: null,
    openai: null,
    openrouter: null,
  });
  const providerApiKeyChangesRef = useRef<Record<string, { version: number; value: string | null }>>({});

  // Ollama models list and error state
  const [models, setModels] = useState<OllamaModel[]>([]);
  const [error, setError] = useState<string>('');

  // Device configuration state
  const [selectedDevices, setSelectedDevices] = useState<SelectedDevices>({
    micDevice: null,
    systemDevice: null
  });

  // Language preference state
  const [selectedLanguage, setSelectedLanguage] = useState<string>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('primaryLanguage');
      return saved || 'auto';
    }
    return 'auto';
  });

  // UI preferences state
  const [showConfidenceIndicator, setShowConfidenceIndicator] = useState<boolean>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('showConfidenceIndicator');
      return saved !== null ? saved === 'true' : true;
    }
    return true;
  });

  // Summary configs
  const [isAutoSummary, setisAutoSummary] = useState<boolean>(() => {
    if (typeof window !== 'undefined') {
      const saved = localStorage.getItem('isAutoSummary');
      return saved !== null ? saved === 'true' : false
    }
    return false;
  });

  // Beta features state (localStorage)
  const [betaFeatures, setBetaFeatures] = useState<BetaFeatures>(() => {
    return loadBetaFeatures();
  });

  // Speaker diarization settings state
  const [diarizationSettings, setDiarizationSettings] = useState<DiarizationSettings>(DEFAULT_DIARIZATION_SETTINGS);
  const savedDiarizationSettingsRef = useRef<DiarizationSettings>(DEFAULT_DIARIZATION_SETTINGS);
  const diarizationSaveQueueRef = useRef<Promise<void>>(Promise.resolve());
  const diarizationSaveVersionRef = useRef(0);
  const [isLoadingDiarizationSettings, setIsLoadingDiarizationSettings] = useState(false);

  // Preference settings state (lazy loaded)
  const [notificationSettings, setNotificationSettings] = useState<NotificationSettings | null>(null);
  const [storageLocations, setStorageLocations] = useState<StorageLocations | null>(null);
  const [isLoadingPreferences, setIsLoadingPreferences] = useState(false);
  const preferencesLoadedRef = useRef(false);
  const isLoadingRef = useRef(false);

  // Load Ollama models (uses saved endpoint, re-runs when endpoint changes after config load)
  useEffect(() => {
    const loadModels = async () => {
      try {
        const endpoint = modelConfig.ollamaEndpoint || null;
        const modelList = await invoke<OllamaModel[]>('get_ollama_models', { endpoint });
        setModels(modelList);
        setError('');
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load Ollama models');
        console.error('Error loading models:', err);
      }
    };
    loadModels();
  }, [modelConfig.ollamaEndpoint]);

  // Load transcript configuration on mount
  useEffect(() => {
    const loadTranscriptConfig = async () => {
      try {
        const config = await configService.getTranscriptConfig();
        if (config) {
          console.log('[ConfigContext] Loaded saved transcript config:', config);
          setTranscriptModelConfig({
            provider: config.provider || '',
            model: config.model || '',
            apiKey: config.apiKey || null
          });
        }
      } catch (error) {
        console.error('[ConfigContext] Failed to load transcript config:', error);
        toast.error('Could not load transcription settings', { description: String(error) });
      }
    };
    loadTranscriptConfig();
  }, []);

  // Sync language preference to Rust on mount (fixes startup desync bug)
  useEffect(() => {
    if (selectedLanguage) {
      invoke('set_language_preference', { language: selectedLanguage })
        .then(() => {
          console.log('[ConfigContext] Synced language preference to Rust on startup:', selectedLanguage);
        })
        .catch(err => {
          console.error('[ConfigContext] Failed to sync language preference to Rust on startup:', err);
        });
    }
  }, []); 

  // Load model configuration on mount
  useEffect(() => {
    const fetchModelConfig = async () => {
      try {
        const data = await configService.getModelConfig();
        if (data && data.provider) {
          // If provider is custom-openai, fetch the additional config
          if (data.provider === 'custom-openai') {
            try {
              const customConfig = await configService.getCustomOpenAIConfig();
              if (customConfig) {
                // Merge custom config fields into modelConfig
                console.log('[ConfigContext] Loading custom OpenAI config:', {
                  endpoint: customConfig.endpoint,
                  model: customConfig.model,
                });
                const resolvedModel = customConfig.model || data.model || '';
                setModelConfig(prev => ({
                  ...prev,
                  provider: data.provider,
                  model: resolvedModel || prev.model,
                  whisperModel: data.whisperModel || prev.whisperModel,
                  customOpenAIEndpoint: customConfig.endpoint,
                  customOpenAIModel: customConfig.model,
                  customOpenAIApiKey: customConfig.apiKey,
                  maxTokens: customConfig.maxTokens,
                  temperature: customConfig.temperature,
                  topP: customConfig.topP,
                }));

                // Seed per-provider model cache from DB
                if (resolvedModel) {
                  const map = JSON.parse(localStorage.getItem('providerModelMap') || '{}');
                  map[data.provider] = resolvedModel;
                  localStorage.setItem('providerModelMap', JSON.stringify(map));
                }

                return; // Early return
              }
            } catch (err) {
              console.error('[ConfigContext] Failed to fetch custom OpenAI config:', err);
            }
          }

          // For non-custom-openai providers, just set base config
          setModelConfig(prev => ({
            ...prev,
            provider: data.provider,
            model: data.model || prev.model,
            whisperModel: data.whisperModel || prev.whisperModel,
            ollamaEndpoint: data.ollamaEndpoint,
          }));

          // Seed per-provider model cache from DB
          if (data.model) {
            const map = JSON.parse(localStorage.getItem('providerModelMap') || '{}');
            map[data.provider] = data.model;
            localStorage.setItem('providerModelMap', JSON.stringify(map));
          }
        }
      } catch (error) {
        console.error('Failed to fetch saved model config in ConfigContext:', error);
      } finally {
        setIsModelConfigLoading(false);
      }
    };
    fetchModelConfig();
  }, []);

  // Load all provider API keys on mount
  useEffect(() => {
    const loadAllApiKeys = async () => {
      try {
        const providers = ['claude', 'groq', 'openai', 'openrouter'] as const;
        const changes = { ...providerApiKeyChangesRef.current };
        const keys = await Promise.all(
          providers.map(p =>
            invoke<string>('api_get_api_key', { provider: p })
              .catch(() => null) // Gracefully handle missing keys
          )
        );

        setProviderApiKeys(previous => {
          const next = { ...previous };
          providers.forEach((provider, index) => {
            // Do not restore a key changed while this startup request was pending.
            if (changes[provider] === providerApiKeyChangesRef.current[provider]) {
              next[provider] = keys[index] || null;
            }
          });
          return next;
        });
        console.log('[ConfigContext] Loaded provider API keys');
      } catch (error) {
        console.error('[ConfigContext] Failed to load provider API keys:', error);
      }
    };

    loadAllApiKeys();
  }, []);

  // Listen for model config updates from other components
  useEffect(() => {
    const setupListener = async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<ModelConfig>('model-config-updated', (event) => {
        console.log('[ConfigContext] Received model-config-updated event:', event.payload);
        setModelConfig(event.payload);

        // Update provider-specific key when config changes
        if (event.payload.apiKey && event.payload.provider !== 'custom-openai') {
          updateProviderApiKey(event.payload.provider, event.payload.apiKey);
        }
      });
      return unlisten;
    };

    let cleanup: (() => void) | undefined;
    setupListener().then(fn => cleanup = fn);

    return () => {
      cleanup?.();
    };
  }, []);

  // Load device preferences on mount
  useEffect(() => {
    const loadDevicePreferences = async () => {
      try {
        const prefs = await configService.getRecordingPreferences();
        if (prefs && (prefs.preferred_mic_device || prefs.preferred_system_device)) {
          setSelectedDevices({
            micDevice: prefs.preferred_mic_device,
            systemDevice: prefs.preferred_system_device
          });
          console.log('Loaded device preferences:', prefs);
        }
      } catch (error) {
        console.log('No device preferences found or failed to load:', error);
      }
    };
    loadDevicePreferences();
  }, []);

  // Load speaker diarization settings on mount
  useEffect(() => {
    const loadDiarizationSettings = async () => {
      setIsLoadingDiarizationSettings(true);
      try {
        const settings = await configService.getDiarizationSettings();
        savedDiarizationSettingsRef.current = settings;
        setDiarizationSettings(settings);
      } catch (error) {
        console.error('[ConfigContext] Failed to load diarization settings:', error);
        savedDiarizationSettingsRef.current = DEFAULT_DIARIZATION_SETTINGS;
        setDiarizationSettings(DEFAULT_DIARIZATION_SETTINGS);
      } finally {
        setIsLoadingDiarizationSettings(false);
      }
    };

    loadDiarizationSettings();
  }, []);

  // Calculate model options based on available models
  const modelOptions: Record<ModelConfig['provider'], string[]> = {
    ollama: models.map(model => model.name),
    claude: ['claude-3-5-sonnet-latest'],
    groq: ['llama-3.3-70b-versatile'],
    openrouter: [],
    openai: ['gpt-4', 'gpt-4-turbo', 'gpt-3.5-turbo'],
    'builtin-ai': [],
    'custom-openai': [],
  };

  // Toggle confidence indicator with localStorage persistence
  const toggleConfidenceIndicator = useCallback((checked: boolean) => {
    setShowConfidenceIndicator(checked);
    if (typeof window !== 'undefined') {
      localStorage.setItem('showConfidenceIndicator', checked.toString());
    }
    // Trigger a custom event to notify other components
    window.dispatchEvent(new CustomEvent('confidenceIndicatorChanged', { detail: checked }));
  }, []);

  const toggleIsAutoSummary = useCallback((checked: boolean) => {
    setisAutoSummary(checked);
    if (typeof window !== 'undefined') {
      localStorage.setItem('isAutoSummary', checked.toString());
    }
  }, [])

  // Toggle beta feature with localStorage persistence
  const toggleBetaFeature = useCallback((featureKey: BetaFeatureKey, enabled: boolean) => {
    setBetaFeatures(prev => {
      const updated = { ...prev, [featureKey]: enabled };
      saveBetaFeatures(updated);
      return updated;
    });
  }, []);

  const updateDiarizationSettings = useCallback(async (settings: DiarizationSettings) => {
    const version = ++diarizationSaveVersionRef.current;
    setDiarizationSettings(settings);
    // Save in order so an older request cannot overwrite a newer setting.
    const save = diarizationSaveQueueRef.current.then(async () => {
      await configService.saveDiarizationSettings(settings);
      savedDiarizationSettingsRef.current = settings;
    });
    diarizationSaveQueueRef.current = save.catch(() => {});
    try {
      await save;
    } catch (error) {
      // An older failure must not replace a newer edit that is still pending.
      if (version === diarizationSaveVersionRef.current) {
        setDiarizationSettings(savedDiarizationSettingsRef.current);
      }
      throw error;
    }
  }, []);

  // Update individual provider API key
  const updateProviderApiKey = useCallback((provider: string, apiKey: string | null) => {
    providerApiKeyChangesRef.current[provider] = {
      version: (providerApiKeyChangesRef.current[provider]?.version ?? 0) + 1, value: apiKey,
    };
    setProviderApiKeys(prev => ({ ...prev, [provider]: apiKey }));
  }, []);

  const getProviderApiKeyChange = useCallback((provider: string) =>
    providerApiKeyChangesRef.current[provider] ?? { version: 0, value: null }, []);

  // Settings can be open in more than one place. Do not let a save started
  // in one view restore a key removed from another view.
  const runModelSettingsOperation = useCallback(async (operation: () => Promise<void>) => {
    if (modelSettingsBusyRef.current) return false;
    modelSettingsBusyRef.current = true;
    setIsModelSettingsBusy(true);
    try {
      await operation();
      return true;
    } finally {
      modelSettingsBusyRef.current = false;
      setIsModelSettingsBusy(false);
    }
  }, []);

  // Lazy load preference settings (only loads if not already cached)
  const loadPreferences = useCallback(async () => {
    // If already loaded, don't reload
    if (preferencesLoadedRef.current) {
      return;
    }

    // If currently loading, don't start another load
    if (isLoadingRef.current) {
      return;
    }

    isLoadingRef.current = true;
    setIsLoadingPreferences(true);
    try {
      // Load notification settings from backend
      let settings: NotificationSettings | null = null;
      try {
        settings = await invoke<NotificationSettings>('get_notification_settings');
        setNotificationSettings(settings);
      } catch (notifError) {
        console.error('[ConfigContext] Failed to load notification settings:', notifError);
        // Use default values if notification settings fail to load
        setNotificationSettings(null);
      }

      // Load storage locations
      const [dbDir, modelsDir, recordingsDir] = await Promise.all([
        invoke<string>('get_database_directory'),
        invoke<string>('whisper_get_models_directory'),
        invoke<string>('get_default_recordings_folder_path')
      ]);

      setStorageLocations({
        database: dbDir,
        models: modelsDir,
        recordings: recordingsDir
      });

      // Mark as loaded
      preferencesLoadedRef.current = true;
    } catch (error) {
      console.error('[ConfigContext] Failed to load preferences:', error);
    } finally {
      isLoadingRef.current = false;
      setIsLoadingPreferences(false);
    }
  }, []);

  // Update notification settings
  const updateNotificationSettings = useCallback(async (settings: NotificationSettings) => {
    try {
      await invoke('set_notification_settings', { settings });
      setNotificationSettings(settings);
    } catch (error) {
      console.error('[ConfigContext] Failed to update notification settings:', error);
      throw error; // Re-throw so component can handle error
    }
  }, []);

  // Wrapper for setSelectedLanguage that persists to localStorage and syncs to Rust
  const handleSetSelectedLanguage = useCallback((lang: string) => {
    setSelectedLanguage(lang);
    if (typeof window !== 'undefined') {
      localStorage.setItem('primaryLanguage', lang);
    }
    // Sync with Rust in-memory state for live recording
    invoke('set_language_preference', { language: lang }).catch(err =>
      console.error('Failed to sync language preference to Rust:', err)
    );
  }, []);

  // FUN-ASR rejects other languages, and recording would refuse to start, so
  // fall back to auto-detect when it is selected with an unsupported language.
  useEffect(() => {
    if (transcriptModelConfig.provider === 'nemotron' && !NEMOTRON_SUPPORTED_LANGUAGES.has(selectedLanguage)) {
      handleSetSelectedLanguage('auto');
    }
    if (
      transcriptModelConfig.provider === 'funasr'
      && !FUNASR_SUPPORTED_LANGUAGES.has(selectedLanguage)
    ) {
      handleSetSelectedLanguage('auto');
      toast.info('Transcription language set to Auto Detect', {
        description: 'FUN-ASR Nano supports auto-detect, English, Chinese and Japanese.',
      });
    }
  }, [transcriptModelConfig.provider, selectedLanguage, handleSetSelectedLanguage]);

  const value: ConfigContextType = useMemo(() => ({
    modelConfig,
    setModelConfig,
    isModelConfigLoading,
    isModelSettingsBusy,
    runModelSettingsOperation,
    isAutoSummary,
    toggleIsAutoSummary,
    providerApiKeys,
    updateProviderApiKey,
    getProviderApiKeyChange,
    transcriptModelConfig,
    setTranscriptModelConfig,
    selectedDevices,
    setSelectedDevices,
    selectedLanguage,
    setSelectedLanguage: handleSetSelectedLanguage,
    showConfidenceIndicator,
    toggleConfidenceIndicator,
    betaFeatures,
    toggleBetaFeature,
    diarizationSettings,
    isLoadingDiarizationSettings,
    updateDiarizationSettings,
    models,
    modelOptions,
    error,
    notificationSettings,
    storageLocations,
    isLoadingPreferences,
    loadPreferences,
    updateNotificationSettings,
  }), [
    modelConfig,
    isModelConfigLoading,
    isModelSettingsBusy,
    runModelSettingsOperation,
    isAutoSummary,
    toggleIsAutoSummary,
    providerApiKeys,
    updateProviderApiKey,
    getProviderApiKeyChange,
    transcriptModelConfig,
    selectedDevices,
    selectedLanguage,
    handleSetSelectedLanguage,
    showConfidenceIndicator,
    toggleConfidenceIndicator,
    betaFeatures,
    toggleBetaFeature,
    diarizationSettings,
    isLoadingDiarizationSettings,
    updateDiarizationSettings,
    models,
    modelOptions,
    error,
    notificationSettings,
    storageLocations,
    isLoadingPreferences,
    loadPreferences,
    updateNotificationSettings,
  ]);

  return (
    <ConfigContext.Provider value={value}>
      {children}
    </ConfigContext.Provider>
  );
}

export function useConfig() {
  const context = useContext(ConfigContext);
  if (context === undefined) {
    throw new Error('useConfig must be used within a ConfigProvider');
  }
  return context;
}
