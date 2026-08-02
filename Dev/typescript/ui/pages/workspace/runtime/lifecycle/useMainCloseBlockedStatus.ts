import { useEffect } from 'react';
import { listenToMainWindowEvent } from '#core/infrastructure/tauri/mainWindow.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export function useMainCloseBlockedStatus(setStatus: StatusApi['setMessage']) {
  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    void listenToMainWindowEvent<string>('main-close-blocked', (event) => {
      setStatus(event.payload, 'err');
    }).then((unlisten) => {
      if (disposed) unlisten();
      else dispose = unlisten;
    }).catch((error) => {
      if (!disposed) setStatus(`Could not subscribe to close-blocked status: ${error}`, 'err');
    });
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [setStatus]);
}
