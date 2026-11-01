'use client';

import { ErrorScreen } from '@/components/ErrorScreen';

// Catches a crash in any page so the sidebar and providers in the layout stay alive.
export default function Error({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
  return <ErrorScreen error={error} reset={reset} />;
}
