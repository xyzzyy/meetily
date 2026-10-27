'use client';

import { useEffect } from 'react';

interface ErrorScreenProps {
  error: Error & { digest?: string };
  reset: () => void;
}

/** Recovery screen shown instead of a blank window when a page crashes while rendering. */
export function ErrorScreen({ error, reset }: ErrorScreenProps) {
  useEffect(() => {
    console.error('Unhandled UI error:', error);
  }, [error]);

  return (
    <div
      role="alert"
      className="flex h-screen w-full flex-col items-center justify-center gap-4 bg-white p-8 text-center dark:bg-ink-950"
    >
      <h1 className="text-xl font-semibold text-gray-900 dark:text-ink-100">Something went wrong</h1>
      <p className="max-w-md text-sm text-gray-600 dark:text-ink-300">
        This screen hit an unexpected error. Try again, and restart the app if it keeps happening.
      </p>
      {error.message && (
        <pre className="max-w-md overflow-auto rounded-md bg-gray-100 px-3 py-2 text-left text-xs text-gray-700 dark:bg-ink-900 dark:text-ink-300">
          {error.message}
        </pre>
      )}
      <div className="flex gap-3">
        <button
          type="button"
          onClick={reset}
          className="rounded-md bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700"
        >
          Try again
        </button>
        <button
          type="button"
          onClick={() => window.location.assign('/')}
          className="rounded-md border border-gray-300 px-4 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 dark:border-ink-700 dark:text-ink-200 dark:hover:bg-ink-800"
        >
          Go to home
        </button>
      </div>
    </div>
  );
}
