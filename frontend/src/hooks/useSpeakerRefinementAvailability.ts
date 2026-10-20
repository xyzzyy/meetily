import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

// Platform support does not depend on whether the models are downloaded.
export function useSpeakerRefinementAvailability() {
  const [supported, setSupported] = useState<boolean | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let disposed = false;
    void invoke<{ refinementSupported: boolean }>('get_diarization_model_status')
      .then(status => {
        if (!disposed) setSupported(status.refinementSupported === true);
      })
      .catch(error => {
        console.warn('Could not check speaker analysis support:', error);
        if (!disposed) setFailed(true);
      });
    return () => { disposed = true; };
  }, []);

  return {
    supported: supported === true,
    checking: supported === null && !failed,
    unavailableReason: supported === true ? null : failed
      ? 'Could not check speaker analysis support. Reopen this view to try again.'
      : supported === null ? 'Checking speaker analysis support…'
      : 'Speaker analysis requires Apple Silicon, macOS 14.4 or later, and the speaker analysis tool. Live labels and manual corrections are still available.',
  };
}
