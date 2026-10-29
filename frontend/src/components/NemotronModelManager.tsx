import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { confirm } from '@tauri-apps/plugin-dialog';
import { toast } from 'sonner';
import { useModelSelection } from '@/hooks/useModelSelection';
import { Button } from './ui/button';
import { TranscriptionModelCard, modelCardStatus } from './TranscriptionModelCard';

interface Model {
  name: string;
  display_name: string;
  size_mb: number;
  memory: string;
  supported: boolean;
  status: 'Available' | 'Missing' | { Downloading: { progress: number } };
}
interface Progress { model: string; phase: string; percent: number }

export function NemotronModelManager({ selectedModel, onModelSelect, autoSave = false }: {
  selectedModel?: string;
  onModelSelect?: (model: string) => void;
  autoSave?: boolean;
}) {
  const [models, setModels] = useState<Model[]>();
  const [error, setError] = useState<string>();
  const [progress, setProgress] = useState<Progress>();
  const [cancelling, setCancelling] = useState(false);
  const { savingModel, selectModel } = useModelSelection({
    provider: 'nemotron', autoSave, onModelSelect,
    displayName: name => models?.find(model => model.name === name)?.display_name ?? 'Nemotron 3.5 Transcribe',
  });
  const refresh = useCallback(async () => {
    try { setModels(await invoke<Model[]>('nemotron_get_available_models')); }
    catch (e) { setError(String(e)); }
  }, []);
  useEffect(() => {
    let disposed = false;
    const listeners = Promise.all([
      listen<Progress>('nemotron-download-progress', e => { if (!disposed) setProgress(e.payload); }),
      listen('nemotron-download-finished', () => {
        if (!disposed) { setProgress(undefined); setCancelling(false); void refresh(); }
      }),
    ]);
    void refresh();
    return () => { disposed = true; void listeners.then(unlisten => unlisten.forEach(fn => fn())); };
  }, [refresh]);
  const download = async (model: Model) => {
    setProgress({ model: model.name, phase: 'Preparing download', percent: 0 }); setError(undefined);
    try { await invoke('nemotron_download_model', { model: model.name }); }
    catch (e) { setError(String(e)); }
    finally { setProgress(undefined); setCancelling(false); await refresh(); }
  };
  const remove = async (model: Model) => {
    // The saved selection would point at missing files until it is downloaded again.
    if (selectedModel === model.name && model.status === 'Available') {
      toast.error('Select another model before deleting this one');
      return;
    }
    const others = models?.some(other => other.name !== model.name && other.status === 'Available');
    const message = others
      ? `Delete ${model.display_name}? The shared encoder and runtime stay for the other Nemotron model.`
      : `Delete ${model.display_name} with its shared encoder and runtime? This frees about ${Math.round(model.size_mb / 1000)} GB.`;
    if (!await confirm(message, { title: 'Delete Nemotron', kind: 'warning' })) return;
    try { await invoke('nemotron_delete_model', { model: model.name }); await refresh(); }
    catch (e) { toast.error('Could not delete Nemotron', { description: String(e) }); }
  };
  const supported = models?.every(model => model.supported) ?? true;
  return <div className="space-y-3">
    <p className="text-sm text-muted-foreground">NVIDIA&apos;s full 30B-A4B model, run by NVIDIA&apos;s native NeMo-Speech.cpp on the GPU. Audio stays on this device. The model detects the spoken language and, when speaker labels are on, tells speakers apart. Speed depends on your Mac; real-time transcription is not guaranteed.</p>
    <button type="button" className="text-sm underline" onClick={() => void invoke('open_external_url', { url: 'https://huggingface.co/AtomicChat/NVIDIA-Nemotron-3.5-Transcribe-30B-A4B-GGUF' })}>Model and runtime details</button>
    {error && <p role="alert">{error}</p>}
    {!models && <Button onClick={() => { setError(undefined); void refresh(); }}>Load model list</Button>}
    {models && !supported && <p role="status">This runtime requires macOS on Apple silicon.</p>}
    {supported && models?.map(model => {
      const active = progress?.model === model.name;
      return <TranscriptionModelCard key={model.name}
        displayName={model.display_name} selected={selectedModel === model.name}
        saving={savingModel === model.name} selectionLocked={!!savingModel}
        details={[`${(model.size_mb / 1000).toFixed(1)} GB download`, `Apple silicon · ${model.memory}`]}
        status={active ? { kind: 'downloading', progress: progress.percent, label: progress.phase, cancelling } : modelCardStatus(model.status)}
        onSelect={() => selectModel(model.name)}
        onDownload={() => { if (progress) toast.error('Wait for the current Nemotron download to finish'); else return download(model); }}
        onCancel={async () => { setCancelling(true); await invoke('nemotron_cancel_download'); }}
        onDelete={() => remove(model)}
      />;
    })}
  </div>;
}
