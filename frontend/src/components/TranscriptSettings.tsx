import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Input } from './ui/input';
import { Button } from './ui/button';
import { Label } from './ui/label';
import { Eye, EyeOff, Lock, Unlock } from 'lucide-react';
import { ModelManager } from './WhisperModelManager';
import { ParakeetModelManager } from './ParakeetModelManager';
import { FunAsrModelManager } from './FunAsrModelManager';
import { NemotronModelManager } from './NemotronModelManager';
import { SpeakerDiarizationSettings } from './SpeakerDiarizationSettings';


export interface TranscriptModelProps {
    provider: '' | 'localWhisper' | 'parakeet' | 'funasr' | 'nemotron' | 'deepgram' | 'elevenLabs' | 'groq' | 'openai';
    model: string;
    apiKey?: string | null;
}

export interface TranscriptSettingsProps {
    transcriptModelConfig: TranscriptModelProps;
    setTranscriptModelConfig: (config: TranscriptModelProps) => void;
    onModelSelect?: () => void;
}

/** Providers that run on this device and manage their own models. */
const LOCAL_PROVIDERS = new Set(['localWhisper', 'parakeet', 'funasr', 'nemotron']);
const isLocalProvider = (provider: string) => LOCAL_PROVIDERS.has(provider);
type LocalProvider = 'localWhisper' | 'parakeet' | 'funasr' | 'nemotron';

/** What sets each local engine apart, shown above its models. */
const LOCAL_ENGINE_NOTES: Record<LocalProvider, string> = {
    localWhisper: 'Runs on this device and uses GPU acceleration when available. Larger models are more accurate but slower and need more disk space.',
    parakeet: 'Runs on this device and is fast enough for real-time transcription. Precise is larger and more accurate; Lightning and Compact are smaller and faster.',
    nemotron: 'Runs on Apple silicon with NVIDIA\'s native NeMo-Speech.cpp runtime. Q2_0_L needs about 17.2 GB of disk space and a Mac with at least 24 GB of unified memory; IQ4_NL needs about 21.4 GB and at least 32 GB. Loading the model can take a minute or more.',
    funasr: 'Runs on this device on the CPU (no GPU acceleration yet). Supports auto-detect, English, Chinese and Japanese. Needs a one-time download of about 950 MB: a shared speech encoder plus the decoder you choose.',
};

export function TranscriptSettings({ transcriptModelConfig, setTranscriptModelConfig, onModelSelect }: TranscriptSettingsProps) {
    const [apiKey, setApiKey] = useState<string | null>(transcriptModelConfig.apiKey || null);
    const [showApiKey, setShowApiKey] = useState<boolean>(false);
    const [isApiKeyLocked, setIsApiKeyLocked] = useState<boolean>(true);
    const [isLockButtonVibrating, setIsLockButtonVibrating] = useState<boolean>(false);
    const [uiProvider, setUiProvider] = useState<TranscriptModelProps['provider']>(transcriptModelConfig.provider);

    // Sync uiProvider when backend config changes (e.g., after model selection or initial load)
    useEffect(() => {
        setUiProvider(transcriptModelConfig.provider);
    }, [transcriptModelConfig.provider]);

    useEffect(() => {
        if (isLocalProvider(transcriptModelConfig.provider)) {
            setApiKey(null);
        }
    }, [transcriptModelConfig.provider]);

    const fetchApiKey = async (provider: string) => {
        try {

            const data = await invoke('api_get_transcript_api_key', { provider }) as string;

            setApiKey(data || '');
        } catch (err) {
            console.error('Error fetching API key:', err);
            setApiKey(null);
        }
    };
    const modelOptions = {
        localWhisper: [], // Model selection handled by ModelManager component
        parakeet: [], // Model selection handled by ParakeetModelManager component
        funasr: [], // Model selection handled by FunAsrModelManager component
        nemotron: [],
        deepgram: ['nova-2-phonecall'],
        elevenLabs: ['eleven_multilingual_v2'],
        groq: ['llama-3.3-70b-versatile'],
        openai: ['gpt-4o'],
    };
    const requiresApiKey = transcriptModelConfig.provider === 'deepgram' || transcriptModelConfig.provider === 'elevenLabs' || transcriptModelConfig.provider === 'openai' || transcriptModelConfig.provider === 'groq';

    const handleInputClick = () => {
        if (isApiKeyLocked) {
            setIsLockButtonVibrating(true);
            setTimeout(() => setIsLockButtonVibrating(false), 500);
        }
    };

    // The managers save a selection before reporting it; mirror it and close the modal.
    const handleLocalModelSelect = (provider: LocalProvider) => (modelName: string) => {
        setTranscriptModelConfig({
            ...transcriptModelConfig,
            provider,
            model: modelName
        });
        if (onModelSelect) {
            onModelSelect();
        }
    };

    const localManagerProps = (provider: LocalProvider) => ({
        selectedModel: transcriptModelConfig.provider === provider ? transcriptModelConfig.model : undefined,
        onModelSelect: handleLocalModelSelect(provider),
        autoSave: true,
    });

    return (
        <div>
            <div>
                {/* <div className="flex justify-between items-center mb-4">
                    <h3 className="text-lg font-semibold text-gray-900">Transcript Settings</h3>
                </div> */}
                <div className="space-y-4 pb-6">
                    <div>
                        <Label className="block text-sm font-medium text-gray-700 dark:text-ink-200 mb-1">
                            Transcript Model
                        </Label>
                        <div className="flex space-x-2 mx-1">
                            <Select
                                value={uiProvider}
                                onValueChange={(value) => {
                                    const provider = value as TranscriptModelProps['provider'];
                                    setUiProvider(provider);
                                    if (!isLocalProvider(provider)) {
                                        fetchApiKey(provider);
                                    }
                                }}
                            >
                                <SelectTrigger className='focus:ring-1 focus:ring-blue-500 focus:border-blue-500'>
                                    <SelectValue placeholder="Select provider" />
                                </SelectTrigger>
                                <SelectContent>
                                    <SelectItem value="parakeet">⚡ Parakeet (Recommended - Real-time / Accurate)</SelectItem>
                                    <SelectItem value="localWhisper">🏠 Local Whisper (High Accuracy)</SelectItem>
                                    <SelectItem value="funasr">🌏 FUN-ASR Nano (English, Chinese, Japanese)</SelectItem>
                                    <SelectItem value="nemotron">Nemotron 3.5 Transcribe (Apple silicon)</SelectItem>
                                    {/* <SelectItem value="deepgram">☁️ Deepgram (Backup)</SelectItem>
                                    <SelectItem value="elevenLabs">☁️ ElevenLabs</SelectItem>
                                    <SelectItem value="groq">☁️ Groq</SelectItem>
                                    <SelectItem value="openai">☁️ OpenAI</SelectItem> */}
                                </SelectContent>
                            </Select>

                            {!!uiProvider && !isLocalProvider(uiProvider) && (
                                <Select
                                    value={transcriptModelConfig.model}
                                    onValueChange={(value) => {
                                        const model = value as TranscriptModelProps['model'];
                                        setTranscriptModelConfig({ ...transcriptModelConfig, provider: uiProvider, model });
                                    }}
                                >
                                    <SelectTrigger className='focus:ring-1 focus:ring-blue-500 focus:border-blue-500'>
                                        <SelectValue placeholder="Select model" />
                                    </SelectTrigger>
                                    <SelectContent>
                                        {modelOptions[uiProvider].map((model) => (
                                            <SelectItem key={model} value={model}>{model}</SelectItem>
                                        ))}
                                    </SelectContent>
                                </Select>
                            )}

                        </div>
                    </div>

                    {!transcriptModelConfig.provider && (
                        <p className="text-sm text-muted-foreground" role="status">
                            No transcription model is selected. Choose a provider, then select a model.
                        </p>
                    )}

                    {!!uiProvider && isLocalProvider(uiProvider) && (
                        <div className="mt-6 space-y-3">
                            <p className="text-sm text-muted-foreground">
                                {LOCAL_ENGINE_NOTES[uiProvider as LocalProvider]}
                            </p>
                            {uiProvider === 'localWhisper' && <ModelManager {...localManagerProps('localWhisper')} />}
                            {uiProvider === 'parakeet' && <ParakeetModelManager {...localManagerProps('parakeet')} />}
                            {uiProvider === 'nemotron' && <NemotronModelManager {...localManagerProps('nemotron')} />}
                            {uiProvider === 'funasr' && <FunAsrModelManager {...localManagerProps('funasr')} />}
                        </div>
                    )}


                    {requiresApiKey && (
                        <div>
                            <Label className="block text-sm font-medium text-gray-700 dark:text-ink-200 mb-1">
                                API Key
                            </Label>
                            <div className="relative mx-1">
                                <Input
                                    type={showApiKey ? "text" : "password"}
                                    className={`pr-24 focus:ring-1 focus:ring-blue-500 focus:border-blue-500 ${isApiKeyLocked ? 'bg-gray-100 dark:bg-ink-800 cursor-not-allowed' : ''
                                        }`}
                                    value={apiKey || ''}
                                    onChange={(e) => setApiKey(e.target.value)}
                                    disabled={isApiKeyLocked}
                                    onClick={handleInputClick}
                                    placeholder="Enter your API key"
                                />
                                {isApiKeyLocked && (
                                    <div
                                        onClick={handleInputClick}
                                        className="absolute inset-0 flex items-center justify-center bg-gray-100 dark:bg-ink-800 bg-opacity-50 rounded-md cursor-not-allowed"
                                    />
                                )}
                                <div className="absolute inset-y-0 right-0 pr-1 flex items-center">
                                    <Button
                                        type="button"
                                        variant="ghost"
                                        size="icon"
                                        onClick={() => setIsApiKeyLocked(!isApiKeyLocked)}
                                        className={`transition-colors duration-200 ${isLockButtonVibrating ? 'animate-vibrate text-red-500 dark:text-red-400' : ''
                                            }`}
                                        title={isApiKeyLocked ? "Unlock to edit" : "Lock to prevent editing"}
                                    >
                                        {isApiKeyLocked ? <Lock className="h-4 w-4" /> : <Unlock className="h-4 w-4" />}
                                    </Button>
                                    <Button
                                        type="button"
                                        variant="ghost"
                                        size="icon"
                                        onClick={() => setShowApiKey(!showApiKey)}
                                    >
                                        {showApiKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                                    </Button>
                                </div>
                            </div>
                        </div>
                    )}

                    <SpeakerDiarizationSettings />
                </div>
            </div>
        </div >
    )
}







