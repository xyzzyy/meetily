'use client';

import { useTheme } from '@/contexts/ThemeContext';
import { isTheme } from '@/lib/theme';
import { SettingsSection } from './settings/SettingsSection';

export function ThemeSettings() {
  const { theme, setTheme } = useTheme();

  return (
    <SettingsSection title="Appearance">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <label htmlFor="theme" className="font-medium text-foreground">Theme</label>
          <p id="theme-description" className="mt-1 text-sm text-muted-foreground">
            Select System to use your device theme.
          </p>
        </div>
        <select
          id="theme"
          aria-describedby="theme-description"
          value={theme}
          onChange={(event) => {
            if (isTheme(event.target.value)) setTheme(event.target.value);
          }}
          className="h-9 min-w-36 rounded-md border border-input bg-background px-3 text-sm text-foreground shadow-sm focus:outline-none focus:ring-2 focus:ring-ring"
        >
          <option value="system">System</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </div>
    </SettingsSection>
  );
}
