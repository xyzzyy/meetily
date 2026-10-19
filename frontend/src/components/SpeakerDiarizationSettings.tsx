'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { message } from '@tauri-apps/plugin-dialog';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import { SettingsSection } from '@/components/settings/SettingsSection';
import { SettingsRow } from '@/components/settings/SettingsRow';
import { SettingsStatusBadge } from '@/components/settings/SettingsStatusBadge';
import { useConfig } from '@/contexts/ConfigContext';
import { RememberedVoices } from '@/components/RememberedVoices';

export function SpeakerDiarizationSettings() {
  const [modelInstalled, setModelInstalled] = useState(false);
  const [modelBusy, setModelBusy] = useState(false);
  const [downloadPercent, setDownloadPercent] = useState<number | null>(null);
  const {
    diarizationSettings,
    isLoadingDiarizationSettings,
    updateDiarizationSettings,
  } = useConfig();

  useEffect(() => {
    invoke<{ installed: boolean }>('get_diarization_model_status')
      .then(status => setModelInstalled(status.installed))
      .catch(error => console.error('[SpeakerDiarizationSettings] Model status failed:', error));

    const unlistenPromise = listen<{ percent: number }>('diarization-model-download-progress', event => {
      setDownloadPercent(event.payload.percent);
    });
    return () => { unlistenPromise.then(unlisten => unlisten()); };
  }, []);

  const downloadModel = async () => {
    setModelBusy(true);
    setDownloadPercent(0);
    try {
      await invoke('download_diarization_model');
      setModelInstalled(true);
      setDownloadPercent(null);
    } catch (error) {
      setDownloadPercent(null);
      await message(String(error), { title: 'Speaker model download failed', kind: 'error' });
    } finally {
      setModelBusy(false);
    }
  };

  const deleteModel = async () => {
    setModelBusy(true);
    try {
      await invoke('delete_diarization_model');
      setModelInstalled(false);
    } catch (error) {
      await message(String(error), { title: 'Could not delete speaker model', kind: 'error' });
    } finally {
      setModelBusy(false);
    }
  };

  const update = (patch: Partial<typeof diarizationSettings>) => {
    updateDiarizationSettings({ ...diarizationSettings, ...patch })
      .catch(error => {
        console.error('[SpeakerDiarizationSettings] Failed to save:', error);
        void message(String(error), { title: 'Could not save speaker label settings', kind: 'error' });
      });
  };

  const showLiveLabels = diarizationSettings.showProvisionalLabels
    && diarizationSettings.mode !== 'post_call_only';
  const handleEnabledChange = (enabled: boolean) => update({
    enabled,
    mode: showLiveLabels ? 'live_plus_post_call' : 'post_call_only',
  });

  return (
    <SettingsSection
      title="Speaker labels"
      description="Identify who spoke. You can correct speaker labels after the meeting."
      badge={<SettingsStatusBadge tone="beta">Beta</SettingsStatusBadge>}
    >
      <SettingsRow
        title="Local speaker model"
        description={modelInstalled ? 'Installed locally.' : 'Required to identify speakers and remember voices.'}
        control={modelInstalled ? (
          <div className="flex items-center gap-2">
            <SettingsStatusBadge tone="local">Installed</SettingsStatusBadge>
            <Button variant="outline" size="sm" disabled={modelBusy} onClick={deleteModel}>Delete</Button>
          </div>
        ) : (
          <Button size="sm" disabled={modelBusy} onClick={downloadModel}>
            {downloadPercent !== null ? `Downloading ${downloadPercent}%` : 'Download model'}
          </Button>
        )}
      />

      <SettingsRow
        title="Enable speaker labels"
        description="Separate speakers in the transcript. Remembered voices can use names from earlier meetings."
        control={
          <Switch
            checked={diarizationSettings.enabled}
            disabled={isLoadingDiarizationSettings || (!modelInstalled && !diarizationSettings.enabled)}
            onCheckedChange={handleEnabledChange}
          />
        }
      />

      <SettingsRow
        title="Show speaker labels during recording"
        description="Show temporary labels during recording. Turn this off to hide all live speaker labels."
        control={
          <Switch
            checked={showLiveLabels}
            disabled={!diarizationSettings.enabled || isLoadingDiarizationSettings}
            onCheckedChange={(showProvisionalLabels) => update({
              showProvisionalLabels,
              mode: showProvisionalLabels ? 'live_plus_post_call' : 'post_call_only',
            })}
          />
        }
      />

      <SettingsRow
        title="Run post-call refinement automatically"
        description="Identify speakers when recording stops. If this is off, use Identify speakers in the meeting transcript."
        control={
          <Switch
            checked={diarizationSettings.postCallRefinementEnabled}
            disabled={!diarizationSettings.enabled || isLoadingDiarizationSettings}
            onCheckedChange={(postCallRefinementEnabled) => update({ postCallRefinementEnabled })}
          />
        }
      />

      <p className="text-sm text-muted-foreground">
        After a meeting, use Correct rows, Attribute or Merge in its transcript to fix who said what.
      </p>

      <RememberedVoices />
    </SettingsSection>
  );
}
