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
        title="Mode"
        description="Choose when to identify speakers. Post-call only adds no speaker labels during recording."
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
              <SelectItem value="live_plus_post_call">During and after the meeting</SelectItem>
              <SelectItem value="post_call_only">After the meeting only</SelectItem>
              <SelectItem value="off">Off</SelectItem>
            </SelectContent>
          </Select>
        }
      />

      <SettingsRow
        title="Show provisional labels during call"
        description="Show temporary labels during recording. Turn this off to hide all live speaker labels."
        disabledReason={!liveModeEnabled ? 'Available only in During and after the meeting mode.' : undefined}
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
        description="Identify speakers when recording stops. If this is off, use Identify speakers in the meeting transcript."
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

      <p className="text-sm text-muted-foreground">
        After processing, use Correct rows to change speaker labels or mark multiple speakers.
        Use Rename or Merge to change a speaker across the meeting.
        Remember voice saves a local voice profile for future meetings.
      </p>

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
