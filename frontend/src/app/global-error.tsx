'use client';

import './globals.css';
import { ErrorScreen } from '@/components/ErrorScreen';
import { themeInitScript } from '@/lib/theme';

// Last resort when the root layout itself crashes. It replaces the layout, so it
// supplies its own document and the saved theme.
export default function GlobalError({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script id="theme-init" dangerouslySetInnerHTML={{ __html: themeInitScript }} />
      </head>
      <body className="font-sans antialiased">
        <ErrorScreen error={error} reset={reset} />
      </body>
    </html>
  );
}
