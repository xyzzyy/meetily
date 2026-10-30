'use client';

import './globals.css';
import { useEffect } from 'react';
import { ErrorScreen } from '@/components/ErrorScreen';
import { isTheme, SYSTEM_THEME_QUERY, THEME_STORAGE_KEY, type Theme } from '@/lib/theme';

// Last resort when the root layout itself crashes. It replaces the layout, so it
// supplies its own document and the saved theme.
export default function GlobalError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  useEffect(() => {
    // React does not run inline scripts when it replaces the document.
    let theme: Theme = 'system';
    try {
      const saved = window.localStorage.getItem(THEME_STORAGE_KEY);
      if (isTheme(saved)) theme = saved;
    } catch {}
    const dark = theme === 'dark' || (theme === 'system' && window.matchMedia(SYSTEM_THEME_QUERY).matches);
    document.documentElement.classList.toggle('dark', dark);
    document.documentElement.style.colorScheme = dark ? 'dark' : 'light';
  }, []);

  return (
    <html lang="en" suppressHydrationWarning>
      <head />
      <body className="font-sans antialiased">
        <ErrorScreen error={error} reset={reset} />
      </body>
    </html>
  );
}
