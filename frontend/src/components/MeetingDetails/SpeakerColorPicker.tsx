"use client";

import { useState } from 'react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import palette from '@/lib/speaker-colors.json';

interface SpeakerColorPickerProps {
  name: string;
  color: string;
  disabled: boolean;
  usedColors: Map<string, string>;
  onChange: (color: string) => Promise<boolean>;
}

export function SpeakerColorPicker({ name, color, disabled, usedColors, onChange }: SpeakerColorPickerProps) {
  const [open, setOpen] = useState(false);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button type="button" disabled={disabled}
          aria-label={`Change color for ${name}`} title={`Change color for ${name}`}
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50">
          <span className="h-2.5 w-2.5 rounded-full" style={{ backgroundColor: color }} />
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-48 p-3" aria-label={`Color for ${name}`}>
        <p className="mb-2 text-xs font-medium">Speaker color</p>
        <div className="grid grid-cols-4 gap-2">
          {palette.map(choice => {
            const owner = usedColors.get(choice.value);
            const selected = choice.value === color;
            return (
              <button key={choice.value} type="button"
                aria-label={choice.name} aria-pressed={selected}
                title={owner ? `${choice.name} — used by ${owner}` : choice.name}
                disabled={disabled || !!owner}
                className="flex h-7 w-7 items-center justify-center rounded-full border border-black/10 dark:border-white/10 text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:opacity-25"
                style={{ backgroundColor: choice.value }}
                onClick={async () => { if (await onChange(choice.value)) setOpen(false); }}>
                {selected && <svg viewBox="0 0 16 16" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true"><path d="m3 8 3 3 7-7" /></svg>}
              </button>
            );
          })}
        </div>
      </PopoverContent>
    </Popover>
  );
}
