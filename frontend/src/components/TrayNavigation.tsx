'use client';

import { useEffect } from 'react';
import { usePathname, useRouter } from 'next/navigation';

/** Keep the root save handler and Retry notices alive during tray navigation. */
export function TrayNavigation() {
  const router = useRouter();
  const pathname = usePathname();

  useEffect(() => {
    const openSettings = () => router.push('/settings');
    const startRecording = () => {
      if (pathname === '/') {
        window.dispatchEvent(new CustomEvent('start-recording-from-sidebar'));
      } else {
        sessionStorage.setItem('autoStartRecording', 'true');
        router.push('/');
      }
    };

    window.addEventListener('open-settings-from-tray', openSettings);
    window.addEventListener('start-recording-from-tray', startRecording);
    return () => {
      window.removeEventListener('open-settings-from-tray', openSettings);
      window.removeEventListener('start-recording-from-tray', startRecording);
    };
  }, [pathname, router]);

  return null;
}
