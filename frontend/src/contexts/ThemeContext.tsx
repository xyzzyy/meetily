'use client';

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTheme, SYSTEM_THEME_QUERY, THEME_STORAGE_KEY, type ResolvedTheme, type Theme } from '@/lib/theme';

interface ThemeContextValue {
  theme: Theme;
  resolvedTheme: ResolvedTheme;
  setTheme: (theme: Theme) => void;
}

const ThemeContext = createContext<ThemeContextValue | undefined>(undefined);

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState<Theme>('system');
  const [resolvedTheme, setResolvedTheme] = useState<ResolvedTheme>('light');
  const themeRef = useRef<Theme>('system');

  const applyTheme = useCallback((next: Theme) => {
    const resolved = next === 'system'
      ? (window.matchMedia(SYSTEM_THEME_QUERY).matches ? 'dark' : 'light')
      : next;

    themeRef.current = next;
    document.documentElement.classList.toggle('dark', resolved === 'dark');
    document.documentElement.style.colorScheme = resolved;
    setThemeState(next);
    setResolvedTheme(resolved);
  }, []);

  const setTheme = useCallback((next: Theme) => {
    applyTheme(next);
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
    } catch {
      // The theme still works when local storage is unavailable.
    }
  }, [applyTheme]);

  useEffect(() => {
    let saved: Theme = 'system';
    try {
      const value = window.localStorage.getItem(THEME_STORAGE_KEY);
      if (isTheme(value)) saved = value;
    } catch {}
    applyTheme(saved);

    const media = window.matchMedia(SYSTEM_THEME_QUERY);
    const handleSystemChange = () => applyTheme(themeRef.current);
    const handleStorageChange = (event: StorageEvent) => {
      if (event.key === THEME_STORAGE_KEY || event.key === null) {
        applyTheme(isTheme(event.newValue) ? event.newValue : 'system');
      }
    };

    media.addEventListener('change', handleSystemChange);
    window.addEventListener('storage', handleStorageChange);
    return () => {
      media.removeEventListener('change', handleSystemChange);
      window.removeEventListener('storage', handleStorageChange);
    };
  }, [applyTheme]);

  useEffect(() => {
    if (!isTauri()) return;
    getCurrentWindow().setTheme(theme === 'system' ? null : theme).catch((error) => {
      console.error('Failed to set the window theme:', error);
    });
  }, [theme]);

  const value = useMemo(() => ({ theme, resolvedTheme, setTheme }), [theme, resolvedTheme, setTheme]);

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme() {
  const context = useContext(ThemeContext);
  if (!context) throw new Error('useTheme must be used within ThemeProvider');
  return context;
}
