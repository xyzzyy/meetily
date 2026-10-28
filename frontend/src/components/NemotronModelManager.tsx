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
  supported: boolean;
  status: 'Available' | 'Missing' | { Downloading: { progress: number } };
}
interface Progress { phase: string; percent: number }

export function NemotronModelManager({ selectedModel, onModelSelect, autoSave = false }: {
  selectedModel?: string;
  onModelSelect?: (model: string) => void;
  autoSave?: boolean;
}) {
  const [model, setModel] = useState<Model>();
  const [error, setError] = useState<string>();
  const [progress, setProgress] = useState<Progress>();
  const [cancelling, setCancelling] = useState(false);
  const { savingModel, selectModel } = useModelSelection({ provider: 'nemotron', autoSave, onModelSelect, displayName: () => 'Nemotron 3.5 Transcribe' });
  const refresh = useCallback(async () => {
    try {
      const models = await invoke<Model[]>('nemotron_get_available_models');
      setModel(models[0]);
    } catch (e) { setError(String(e)); }
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
  const download = async () => {
    setProgress({ phase: 'Preparing download', percent: 0 }); setError(undefined);
    try { await invoke('nemotron_download_model'); }
    catch (e) { setError(String(e)); }
    finally { setProgress(undefined); setCancelling(false); await refresh(); }
  };
  const remove = async () => {
    // The saved selection would point at missing files until it is downloaded again.
    if (model && selectedModel === model.name && model.status === 'Available') {
      toast.error('Select another model before deleting this one');
      return;
    }
    if (!await confirm('Delete the Nemotron model and its local runtime? This frees about 20 GB.', { title: 'Delete Nemotron', kind: 'warning' })) return;
    try { await invoke('nemotron_delete_model'); await refresh(); }
    catch (e) { toast.error('Could not delete Nemotron', { description: String(e) }); }
  };
  return <div className="space-y-3">
    <p className="text-sm text-muted-foreground">Full 30B-A4B model with a 4-bit language model. Audio stays on this device. The model detects the spoken language. Speed depends on your Mac; real-time transcription is not guaranteed.</p>
    <a className="text-sm underline" href="https://huggingface.co/nativ-community/Nemotron-3.5-Transcribe-30B-A4B-EA-4bit" target="_blank" rel="noreferrer">Model and runtime details</a>
    {error && <p role="alert">{error}</p>}
    {!model && <Button onClick={() => { setError(undefined); void refresh(); }}>Load model list</Button>}
    {model && !model.supported && <p role="status">This runtime requires macOS on Apple silicon.</p>}
    {model?.supported && <TranscriptionModelCard
      displayName={model.display_name} selected={selectedModel === model.name}
      saving={savingModel === model.name} selectionLocked={!!savingModel}
      details={['19.3 GB model download', 'Apple silicon · 32 GB memory recommended']}
      status={progress ? { kind: 'downloading', progress: progress.percent, label: progress.phase, cancelling } : modelCardStatus(model.status)}
      onSelect={() => selectModel(model.name)} onDownload={download}
      onCancel={async () => { setCancelling(true); await invoke('nemotron_cancel_download'); }}
      onDelete={remove}
    />}
  </div>;
}
