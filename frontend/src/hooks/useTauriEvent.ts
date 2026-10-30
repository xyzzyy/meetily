import { useEffect, useRef } from 'react';
import { listen, type EventCallback } from '@tauri-apps/api/event';

/**
 * Listen to a Tauri event while the component is mounted.
 *
 * `listen` registers asynchronously. If the component unmounts before
 * registration finishes, the listener is removed as soon as it is registered,
 * so it never outlives the component. Each call owns one listener, so a failed
 * registration cannot leave another one behind. The latest `handler` is always
 * called, so passing a new function on every render does not re-register.
 */
export function useTauriEvent<T>(event: string, handler: EventCallback<T>): void {
  const handlerRef = useRef(handler);
  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    listen<T>(event, (received) => {
      if (!disposed) handlerRef.current(received);
    }).then(
      (stop) => {
        if (disposed) stop();
        else unlisten = stop;
      },
      (error) => {
        console.error(`Failed to listen for ${event}:`, error);
      },
    );
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [event]);
}
