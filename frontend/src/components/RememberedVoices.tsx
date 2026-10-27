'use client';

import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { invoke } from '@tauri-apps/api/core';
import { confirm, message } from '@tauri-apps/plugin-dialog';
import { ChevronRight, Pencil, Search, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import type { SpeakerProfile, SpeakerProfileSample } from '@/types';

function initials(name: string): string {
  const words = name.replace(/\(.*?\)/g, ' ').trim().split(/\s+/).filter(Boolean);
  return (words.length > 1 ? words[0][0] + words[words.length - 1][0] : (words[0] ?? '?').slice(0, 2))
    .toUpperCase();
}

function formatSeconds(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return rest ? `${minutes}m ${rest}s` : `${minutes}m`;
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? '' : 's'}`;
}

/** Matches every word typed, in any order and case, e.g. "mad scrum". */
function matchesSearch(name: string, query: string): boolean {
  const haystack = name.toLowerCase();
  return query.toLowerCase().split(/\s+/).filter(Boolean).every(word => haystack.includes(word));
}

/**
 * Voices remembered for recognizing people in future meetings, with search
 * and the meeting recordings that make up each voice.
 */
export function RememberedVoices() {
  const router = useRouter();
  const [profiles, setProfiles] = useState<SpeakerProfile[]>([]);
  const [query, setQuery] = useState('');
  const [expandedProfileId, setExpandedProfileId] = useState<string | null>(null);
  const [samples, setSamples] = useState<SpeakerProfileSample[]>([]);
  const [samplesLoading, setSamplesLoading] = useState(false);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [renamingProfile, setRenamingProfile] = useState<SpeakerProfile | null>(null);
  const [newName, setNewName] = useState('');
  const [renameError, setRenameError] = useState<string | null>(null);

  const loadProfiles = async () => {
    try {
      setProfiles(await invoke<SpeakerProfile[]>('list_speaker_profiles'));
    } catch (error) {
      console.error('[RememberedVoices] Profile list failed:', error);
    }
  };

  useEffect(() => {
    void loadProfiles();
  }, []);

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

  const duplicateNameError = renamingProfile && profiles.some(profile =>
    profile.id !== renamingProfile.id && profile.name.trim().toLowerCase() === newName.trim().toLowerCase())
    ? 'A remembered voice already uses this name. Choose a different name.'
    : null;

  const renameVoice = async () => {
    const name = newName.trim();
    if (!renamingProfile || !name || duplicateNameError || busyKey !== null) return;
    setBusyKey(renamingProfile.id);
    setRenameError(null);
    try {
      await invoke('update_speaker_profile', {
        request: { profileId: renamingProfile.id, name },
      });
      setProfiles(current => current
        .map(profile => profile.id === renamingProfile.id ? { ...profile, name } : profile)
        .sort((a, b) => a.name.localeCompare(b.name)));
      setRenamingProfile(null);
    } catch (error) {
      setRenameError(`Could not rename voice: ${String(error)}`);
    } finally {
      setBusyKey(null);
    }
  };

  const forgetVoice = async (profile: SpeakerProfile) => {
    const confirmed = await confirm(
      `Meetily will stop recognizing ${profile.name} in future meetings. Names already in your transcripts stay as they are.`,
      { title: `Forget ${profile.name}'s voice?`, kind: 'warning', okLabel: 'Forget voice' },
    );
    if (!confirmed) return;
    setBusyKey(profile.id);
    try {
      await invoke('delete_speaker_profile', { profileId: profile.id });
      setProfiles(current => current.filter(item => item.id !== profile.id));
      if (expandedProfileId === profile.id) setExpandedProfileId(null);
    } catch (error) {
      await message(String(error), { title: 'Could not forget voice', kind: 'error' });
    } finally {
      setBusyKey(null);
    }
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
    setBusyKey(key);
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
      setBusyKey(null);
    }
  };

  const visible = profiles.filter(profile => matchesSearch(profile.name, query));

  return (
    <div className="space-y-3 pt-2">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <h4 className="font-medium text-foreground">
            Remembered voices
            {profiles.length > 0 && (
              <span className="ml-2 text-sm font-normal text-muted-foreground">{profiles.length}</span>
            )}
          </h4>
          <p className="mt-1 text-sm text-muted-foreground">
            Stored on this device as voice fingerprints, not audio. Use Attribute on a speaker in a meeting to add or extend a voice.
          </p>
        </div>
        {profiles.length > 0 && (
          <div className="relative w-full sm:w-56">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
            <Input
              type="search"
              aria-label="Search remembered voices"
              placeholder="Search names"
              className="h-9 pl-8"
              value={query}
              onChange={event => setQuery(event.target.value)}
            />
          </div>
        )}
      </div>

      {profiles.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
          No remembered voices yet. Open a meeting, then use Attribute on a speaker and type their name.
        </p>
      ) : visible.length === 0 ? (
        <p className="rounded-lg border border-dashed border-border p-4 text-sm text-muted-foreground">
          No remembered voices match “{query.trim()}”.
        </p>
      ) : (
        <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border">
          {visible.map(profile => {
            const expanded = expandedProfileId === profile.id;
            return (
              <li key={profile.id}>
                <div className="flex items-center gap-3 px-3 py-2.5">
                  <button
                    type="button"
                    className="flex min-w-0 flex-1 items-center gap-3 rounded-md text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    aria-expanded={expanded}
                    aria-controls={`voice-recordings-${profile.id}`}
                    onClick={() => toggleSamples(profile.id)}
                  >
                    <ChevronRight
                      className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform ${expanded ? 'rotate-90' : ''}`}
                      aria-hidden="true"
                    />
                    <span
                      className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-semibold text-muted-foreground"
                      aria-hidden="true"
                    >
                      {initials(profile.name)}
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium text-foreground">{profile.name}</span>
                      <span className="block text-xs text-muted-foreground">
                        {formatSeconds(profile.speechSeconds)} of speech · {plural(profile.recordingCount, 'meeting')}
                      </span>
                    </span>
                  </button>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-8 w-8 shrink-0 p-0 text-muted-foreground"
                    aria-label={`Rename ${profile.name}`}
                    title="Rename"
                    disabled={busyKey !== null}
                    onClick={() => {
                      setRenamingProfile(profile);
                      setNewName(profile.name);
                      setRenameError(null);
                    }}
                  >
                    <Pencil className="h-4 w-4" aria-hidden="true" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-8 w-8 shrink-0 p-0 text-muted-foreground hover:text-destructive"
                    aria-label={`Forget ${profile.name}'s voice`}
                    title="Forget voice"
                    disabled={busyKey !== null}
                    onClick={() => void forgetVoice(profile)}
                  >
                    <Trash2 className="h-4 w-4" aria-hidden="true" />
                  </Button>
                </div>

                {expanded && (
                  <div id={`voice-recordings-${profile.id}`} className="border-t border-border bg-muted/30 px-3 py-2 pl-14">
                    {samples.length === 0 && (
                      <p className="py-1 text-xs text-muted-foreground">
                        {samplesLoading ? 'Loading recordings…' : 'No meeting recordings are linked to this voice.'}
                      </p>
                    )}
                    <ul className="space-y-1">
                      {samples.map(sample => {
                        const key = `${sample.meetingId}/${sample.speakerId}`;
                        const date = sample.meetingCreatedAt ? new Date(sample.meetingCreatedAt) : null;
                        return (
                          <li key={key} className="flex items-center gap-2">
                            <div className="min-w-0 flex-1">
                              {sample.meetingTitle ? (
                                <button
                                  type="button"
                                  className="block max-w-full truncate text-left text-sm text-foreground hover:underline"
                                  title="Open meeting"
                                  onClick={() => router.push(`/meeting-details?id=${sample.meetingId}`)}
                                >
                                  {sample.meetingTitle}
                                </button>
                              ) : (
                                <span className="block truncate text-sm text-muted-foreground">Deleted meeting</span>
                              )}
                              <span className="block text-xs text-muted-foreground">
                                {[
                                  date && !Number.isNaN(date.getTime()) ? date.toLocaleDateString() : null,
                                  sample.speakerLabel ? `as ${sample.speakerLabel}` : null,
                                  sample.removable ? `${formatSeconds(sample.speechSeconds)} of speech` : 'Part of the original voice',
                                ].filter(Boolean).join(' · ')}
                              </span>
                            </div>
                            <Button
                              variant="ghost"
                              size="sm"
                              className="h-7 shrink-0 px-2 text-xs"
                              disabled={!sample.removable || busyKey !== null}
                              title={sample.removable
                                ? "Remove this meeting's speech and rebuild the voice from the rest."
                                : "This recording was blended in before recordings were tracked and can't be removed on its own."}
                              onClick={() => void removeSample(profile, sample)}
                            >
                              {busyKey === key ? 'Removing…' : 'Remove'}
                            </Button>
                          </li>
                        );
                      })}
                    </ul>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {renamingProfile && (
        <Dialog
          open={renamingProfile !== null}
          onOpenChange={open => {
            if (!open && busyKey === null) setRenamingProfile(null);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Rename remembered voice</DialogTitle>
              <DialogDescription>
                Use this name to identify the person in future meetings. Existing transcript names stay the same.
              </DialogDescription>
            </DialogHeader>
            <form className="space-y-4" onSubmit={event => {
              event.preventDefault();
              void renameVoice();
            }}>
              <div className="space-y-2">
                <label htmlFor="remembered-voice-name" className="text-sm font-medium">Name</label>
                <Input
                  id="remembered-voice-name"
                  value={newName}
                  onChange={event => {
                    setNewName(event.target.value);
                    setRenameError(null);
                  }}
                  aria-invalid={Boolean(renameError || duplicateNameError)}
                  aria-describedby={renameError || duplicateNameError ? 'remembered-voice-name-error' : undefined}
                  disabled={busyKey !== null}
                  autoFocus
                  required
                />
                {(renameError || duplicateNameError) && (
                  <p id="remembered-voice-name-error" role="alert" className="text-sm text-destructive">
                    {renameError || duplicateNameError}
                  </p>
                )}
              </div>
              <DialogFooter>
                <Button type="button" variant="outline" disabled={busyKey !== null} onClick={() => setRenamingProfile(null)}>
                  Cancel
                </Button>
                <Button type="submit" disabled={busyKey !== null || Boolean(duplicateNameError) || !newName.trim() || newName.trim() === renamingProfile?.name}>
                  {busyKey === renamingProfile?.id ? 'Saving…' : 'Save'}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}
