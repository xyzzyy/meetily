'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { confirm, message } from '@tauri-apps/plugin-dialog';
import { Switch } from '@/components/ui/switch';
import { Button } from '@/components/ui/button';
import { SettingsSection } from '@/components/settings/SettingsSection';
import { SettingsRow } from '@/components/settings/SettingsRow';
import { SettingsStatusBadge } from '@/components/settings/SettingsStatusBadge';
import { useConfig } from '@/contexts/ConfigContext';
import type { SpeakerProfile, SpeakerProfileSample } from '@/types';

export function SpeakerDiarizationSettings() {
  const [modelInstalled, setModelInstalled] = useState(false);
  const [modelBusy, setModelBusy] = useState(false);
  const [downloadPercent, setDownloadPercent] = useState<number | null>(null);
  const [profiles, setProfiles] = useState<SpeakerProfile[]>([]);
  const [expandedProfileId, setExpandedProfileId] = useState<string | null>(null);
  const [samples, setSamples] = useState<SpeakerProfileSample[]>([]);
  const [samplesLoading, setSamplesLoading] = useState(false);
  const [removingSampleKey, setRemovingSampleKey] = useState<string | null>(null);
  const {
    diarizationSettings,
    isLoadingDiarizationSettings,
    updateDiarizationSettings,
  } = useConfig();

  useEffect(() => {
    invoke<{ installed: boolean }>('get_diarization_model_status')
      .then(status => setModelInstalled(status.installed))
      .catch(error => console.error('[SpeakerDiarizationSettings] Model status failed:', error));
    void loadProfiles();

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

  const loadProfiles = async () => {
    try {
      setProfiles(await invoke<SpeakerProfile[]>('list_speaker_profiles'));
    } catch (error) {
      console.error('[SpeakerDiarizationSettings] Profile list failed:', error);
    }
  };

  const loadSamples = async (profileId: string) => {
    setSamplesLoading(true);
    try {
      setSamples(await invoke<SpeakerProfileSample[]>('list_speaker_profile_samples', { profileId }));
    } catch (error) {
      setSamples([]);
      await message(String(error), { title: 'Could not load recordings', kind: 'error' });
    } finally {
      setSamplesLoading(false);
    }
  };

  const toggleSamples = (profileId: string) => {
    if (expandedProfileId === profileId) {
      setExpandedProfileId(null);
      return;
    }
    setExpandedProfileId(profileId);
    setSamples([]);
    void loadSamples(profileId);
  };

  const removeSample = async (profile: SpeakerProfile, sample: SpeakerProfileSample) => {
    const isLast = samples.filter(item => item.removable).length <= 1;
    if (isLast) {
      const confirmed = await confirm(
        `This is the only recording in ${profile.name}'s remembered voice. Removing it forgets the voice. `
          + 'The speaker keeps their name in the meeting.',
        { title: `Forget ${profile.name}'s voice?`, kind: 'warning', okLabel: 'Remove and forget' },
      );
      if (!confirmed) return;
    }
    const key = `${sample.meetingId}/${sample.speakerId}`;
    setRemovingSampleKey(key);
    try {
      const outcome = await invoke<'recalculated' | 'forgotten'>('remove_speaker_profile_sample', {
        request: { profileId: profile.id, meetingId: sample.meetingId, speakerId: sample.speakerId },
      });
      await loadProfiles();
      if (outcome === 'forgotten') {
        setExpandedProfileId(null);
        setSamples([]);
      } else {
        await loadSamples(profile.id);
      }
    } catch (error) {
      await message(String(error), { title: 'Could not remove recording', kind: 'error' });
    } finally {
      setRemovingSampleKey(null);
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

      <SettingsRow
        title="Overlap handling"
        description="Interruptions are marked as Multiple speakers."
        control={<SettingsStatusBadge tone="local">Multiple speakers</SettingsStatusBadge>}
      />

      <p className="text-sm text-muted-foreground">
        After processing, use Correct rows to change speaker labels or mark multiple speakers.
        Use Attribute or Merge to change a speaker across the meeting.
        Attribute a speaker to a remembered voice to add their speech to it, or type a new name to remember a new voice.
      </p>

      {profiles.map(profile => (
        <div key={profile.id}>
          <SettingsRow
            title={profile.name}
            description={`Remembered locally from ${profile.speechSeconds.toFixed(1)} seconds of speech. No voice clip is stored.`}
            control={
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  aria-expanded={expandedProfileId === profile.id}
                  aria-controls={`voice-recordings-${profile.id}`}
                  onClick={() => toggleSamples(profile.id)}
                >
                  {expandedProfileId === profile.id ? 'Hide recordings' : 'Recordings'}
                </Button>
                <Button variant="outline" size="sm" onClick={() => void deleteProfile(profile.id)}>
                  Forget voice
                </Button>
              </div>
            }
          />
          {expandedProfileId === profile.id && (
            <div id={`voice-recordings-${profile.id}`} className="mt-2 space-y-1 rounded-md border border-border p-2">
              {samples.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  {samplesLoading ? 'Loading recordings…' : 'No meeting recordings are linked to this voice.'}
                </p>
              )}
              {samples.map(sample => {
                const key = `${sample.meetingId}/${sample.speakerId}`;
                const date = sample.meetingCreatedAt ? new Date(sample.meetingCreatedAt) : null;
                return (
                  <div key={key} className="flex items-center gap-2 text-sm">
                    <div className="min-w-0 flex-1">
                      <p className="truncate">{sample.meetingTitle ?? 'Deleted meeting'}</p>
                      <p className="text-xs text-muted-foreground">
                        {[
                          date && !Number.isNaN(date.getTime()) ? date.toLocaleDateString() : null,
                          sample.speakerLabel ? `as ${sample.speakerLabel}` : null,
                          sample.removable ? `${sample.speechSeconds.toFixed(1)}s of speech` : 'Part of the original voice',
                        ].filter(Boolean).join(' · ')}
                      </p>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      disabled={!sample.removable || removingSampleKey !== null}
                      title={sample.removable
                        ? 'Remove this meeting\'s speech and rebuild the voice from the rest.'
                        : 'This recording was blended in before recordings were tracked and can\'t be removed on its own.'}
                      onClick={() => void removeSample(profile, sample)}
                    >
                      {removingSampleKey === key ? 'Removing…' : 'Remove'}
                    </Button>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      ))}
    </SettingsSection>
  );
}
