'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { message } from '@tauri-apps/plugin-dialog';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { SettingsSection } from '@/components/settings/SettingsSection';
import { SettingsRow } from '@/components/settings/SettingsRow';
import { SettingsStatusBadge } from '@/components/settings/SettingsStatusBadge';
import { useConfig } from '@/contexts/ConfigContext';
import type { DiarizationMode } from '@/types/diarization';

export function SpeakerDiarizationSettings() {
  const [modelInstalled, setModelInstalled] = useState(false);
  const [modelBusy, setModelBusy] = useState(false);
  const [downloadPercent, setDownloadPercent] = useState<number | null>(null);
  const [profiles, setProfiles] = useState<Array<{
    id: string; name: string; modelVersion: string; sampleCount: number; speechSeconds: number;
  }>>([]);
  const {
    betaFeatures,
    diarizationSettings,
    isLoadingDiarizationSettings,
    updateDiarizationSettings,
  } = useConfig();

  useEffect(() => {
    invoke<{ installed: boolean }>('get_diarization_model_status')
      .then(status => setModelInstalled(status.installed))
      .catch(error => console.error('[SpeakerDiarizationSettings] Model status failed:', error));
    invoke<typeof profiles>('list_speaker_profiles')
      .then(setProfiles)
      .catch(error => console.error('[SpeakerDiarizationSettings] Profile list failed:', error));

    const unlistenPromise = listen<{ percent: number }>('diarization-model-download-progress', event => {
      setDownloadPercent(event.payload.percent);
    });
    return () => { unlistenPromise.then(unlisten => unlisten()); };
  }, []);

  if (!betaFeatures.speakerDiarization) {
    return null;
  }

  const downloadModel = async () => {
    setModelBusy(true);
    setDownloadPercent(0);
    try {
      await invoke('download_diarization_model');
      setModelInstalled(true);
      setDownloadPercent(null);
    } catch (error) {
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

  const deleteProfile = async (profileId: string) => {
    try {
      await invoke('delete_speaker_profile', { profileId });
      setProfiles(current => current.filter(profile => profile.id !== profileId));
    } catch (error) {
      await message(String(error), { title: 'Could not delete remembered voice', kind: 'error' });
    }
  };

  const update = (patch: Partial<typeof diarizationSettings>) => {
    updateDiarizationSettings({ ...diarizationSettings, ...patch })
      .catch(error => console.error('[SpeakerDiarizationSettings] Failed to save:', error));
  };

  const handleEnabledChange = (enabled: boolean) => {
    if (!enabled) {
      update({ enabled: false, mode: 'off' });
      return;
    }

    update({
      enabled: true,
      mode: diarizationSettings.mode === 'off' ? 'live_plus_post_call' : diarizationSettings.mode,
    });
  };

  const handleModeChange = (mode: DiarizationMode) => {
    if (mode === 'off') {
      update({ mode: 'off', enabled: false });
      return;
    }

    update({ mode, enabled: true });
  };

  const liveModeEnabled = diarizationSettings.enabled && diarizationSettings.mode === 'live_plus_post_call';

  return (
    <SettingsSection
      title="Speaker labels"
      description="Label who spoke in the transcript. Live labels are provisional; final labels are applied after the meeting."
      badge={<SettingsStatusBadge tone="beta">Beta</SettingsStatusBadge>}
    >
      <SettingsRow
        title="Local speaker model"
        description={modelInstalled ? 'Installed locally (28 MB).' : 'Required for live speaker recognition and remembered voices.'}
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
        title="Enable speaker diarization"
        description="Add speaker labels to transcripts without identifying people by voice."
        control={
          <Switch
            checked={diarizationSettings.enabled}
            disabled={isLoadingDiarizationSettings || !modelInstalled}
            onCheckedChange={handleEnabledChange}
          />
        }
      />

      <SettingsRow
        title="Mode"
        description="Recommended: show provisional labels during the call, then refine after recording stops."
        control={
          <Select
            value={diarizationSettings.mode}
            disabled={!diarizationSettings.enabled || isLoadingDiarizationSettings}
            onValueChange={(mode) => handleModeChange(mode as DiarizationMode)}
          >
            <SelectTrigger className="w-[260px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="live_plus_post_call">Live + post-call refinement</SelectItem>
              <SelectItem value="post_call_only">Post-call only</SelectItem>
              <SelectItem value="off">Off</SelectItem>
            </SelectContent>
          </Select>
        }
      />

      <SettingsRow
        title="Show provisional labels during call"
        description="Display live Speaker 1 / Speaker 2 labels with a provisional badge."
        disabledReason={!liveModeEnabled ? 'Available only in Live + post-call refinement mode.' : undefined}
        control={
          <Switch
            checked={diarizationSettings.showProvisionalLabels}
            disabled={!liveModeEnabled || isLoadingDiarizationSettings}
            onCheckedChange={(showProvisionalLabels) => update({ showProvisionalLabels })}
          />
        }
      />

      <SettingsRow
        title="Run post-call refinement automatically"
        description="After the meeting, analyze the full audio and replace provisional labels when quality is good."
        control={
          <Switch
            checked={diarizationSettings.postCallRefinementEnabled}
            disabled={!diarizationSettings.enabled || diarizationSettings.mode === 'off' || isLoadingDiarizationSettings}
            onCheckedChange={(postCallRefinementEnabled) => update({ postCallRefinementEnabled })}
          />
        }
      />

      <SettingsRow
        title="Overlap handling"
        description="Interruptions are marked as Multiple speakers."
        control={<SettingsStatusBadge tone="local">Multiple speakers</SettingsStatusBadge>}
      />

      <SettingsRow
        title="Speaker review"
        description="Allow rename, merge, split, and overlap fixes after post-call processing."
        control={
          <Switch
            checked={diarizationSettings.speakerReviewEnabled}
            disabled={!diarizationSettings.enabled || isLoadingDiarizationSettings}
            onCheckedChange={(speakerReviewEnabled) => update({ speakerReviewEnabled })}
          />
        }
      />

      {profiles.map(profile => (
        <SettingsRow
          key={profile.id}
          title={profile.name}
          description={`Remembered locally from ${profile.speechSeconds.toFixed(1)} seconds of speech. No voice clip is stored.`}
          control={
            <Button variant="outline" size="sm" onClick={() => void deleteProfile(profile.id)}>
              Forget voice
            </Button>
          }
        />
      ))}
    </SettingsSection>
  );
}
