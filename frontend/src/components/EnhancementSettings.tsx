'use client';

import { useEffect, useState } from 'react';
import { toast } from 'sonner';
import { useTranscriptionModels } from '@/hooks/useTranscriptionModels';
import { readEnhancementPreferences, writeEnhancementPreferences, type EnhancementPreferences } from '@/lib/enhancement-preferences';
import { SettingsSection } from './settings/SettingsSection';
import { SettingsRow } from './settings/SettingsRow';
import { Switch } from './ui/switch';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';

export function EnhancementSettings() {
  const [settings, setSettings] = useState<EnhancementPreferences>(readEnhancementPreferences);
  const { availableModels, loadingModels, fetchModels } = useTranscriptionModels(undefined);
  useEffect(() => { void fetchModels(); }, [fetchModels]);
  const update = (next: EnhancementPreferences) => {
    try {
      writeEnhancementPreferences(next);
      setSettings(next);
    } catch (error) {
      toast.error('Could not save enhancement settings', { description: String(error) });
    }
  };
  const selectedKey = settings.model ? `${settings.provider}:${settings.model}` : 'none';
  const available = availableModels.some(model => `${model.provider}:${model.name}` === selectedKey);
  return (
    <SettingsSection title="Enhancement" description="Use a separate model to transcribe saved audio after a call. Download models with the controls above.">
      <SettingsRow title="Default enhancement model" description="This model is also selected when you open Enhance. The live transcription model stays the same."
        control={
          <Select value={selectedKey} onOpenChange={open => { if (open) void fetchModels(); }} onValueChange={key => {
            const model = availableModels.find(item => `${item.provider}:${item.name}` === key);
            update({ ...settings, provider: model?.provider ?? '', model: model?.name ?? '', autoEnhance: model ? settings.autoEnhance : false });
          }}>
            <SelectTrigger aria-label="Default enhancement model"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="none">Select a model</SelectItem>
              {settings.model && !available && <SelectItem value={selectedKey} disabled>{settings.model} (unavailable)</SelectItem>}
              {availableModels.map(model => <SelectItem key={`${model.provider}:${model.name}`} value={`${model.provider}:${model.name}`}>{model.displayName}</SelectItem>)}
            </SelectContent>
          </Select>
        }
      />
      {!loadingModels && !available && <p className="text-sm text-muted-foreground">{settings.autoEnhance ? 'Auto enhance is paused until the selected model is installed.' : 'Select an installed model to use Auto enhance.'}</p>}
      <SettingsRow title="Auto enhance" description="Enhance each recording after it is saved. The enhanced transcript is applied. You can undo it in the meeting."
        control={<Switch aria-label="Auto enhance" checked={settings.autoEnhance} disabled={!available || loadingModels}
          onCheckedChange={autoEnhance => update({ ...settings, autoEnhance })} />}
      />
    </SettingsSection>
  );
}
