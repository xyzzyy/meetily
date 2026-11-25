'use client';

import { useState } from 'react';
import { Button } from '@/components/ui/button';
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '@/components/ui/command';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import type { MeetingSpeaker, SpeakerProfile } from '@/types';

export type SpeakerAttribution = { profileId: string } | { name: string };

interface SpeakerAttributeButtonProps {
  speaker: MeetingSpeaker;
  profiles: SpeakerProfile[];
  disabled: boolean;
  busy: boolean;
  onAttribute: (speaker: MeetingSpeaker, attribution: SpeakerAttribution) => void;
}

/**
 * Attribute a meeting speaker to a person: pick a remembered voice to label the
 * speaker and add their speech to it, or type a new name to remember them.
 */
export function SpeakerAttributeButton({
  speaker,
  profiles,
  disabled,
  busy,
  onAttribute,
}: SpeakerAttributeButtonProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const typedName = query.trim();
  const matchesExisting = profiles.some(
    profile => profile.name.trim().toLowerCase() === typedName.toLowerCase()
  );

  const choose = (attribution: SpeakerAttribution) => {
    setOpen(false);
    setQuery('');
    onAttribute(speaker, attribution);
  };

  return (
    <Popover
      open={open}
      onOpenChange={next => {
        setOpen(next);
        if (!next) setQuery('');
      }}
    >
      <PopoverTrigger asChild>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 shrink-0 px-1.5 text-[11px]"
          title="Name this speaker and remember their voice for future meetings."
          disabled={disabled}
        >
          {busy ? 'Attributing…' : 'Attribute'}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-64 p-0" align="end">
        <Command>
          <CommandInput
            placeholder="Type a name…"
            value={query}
            onValueChange={setQuery}
          />
          <CommandList>
            {!typedName && (
              <CommandEmpty>No remembered voices yet. Type a name to remember this speaker.</CommandEmpty>
            )}
            {typedName && !matchesExisting && (
              <>
                <CommandGroup>
                  <CommandItem
                    forceMount
                    value={`__new__${typedName}`}
                    onSelect={() => choose({ name: typedName })}
                  >
                    Remember “{typedName}” as a new person
                  </CommandItem>
                </CommandGroup>
                {profiles.length > 0 && <CommandSeparator />}
              </>
            )}
            {profiles.length > 0 && (
              <CommandGroup heading="Remembered voices">
                {profiles.map(profile => (
                  <CommandItem
                    key={profile.id}
                    value={`${profile.name} ${profile.id}`}
                    onSelect={() => choose({ profileId: profile.id })}
                  >
                    <span className="truncate">{profile.name}</span>
                    <span className="ml-auto shrink-0 text-[11px] tabular-nums text-muted-foreground">
                      {profile.recognitionReady === false ? 'Name only' : `${Math.round(profile.speechSeconds)}s`}
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
