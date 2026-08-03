import { useCallback, useRef } from 'react';

/**
 * Capture whatever held focus when an overlay mounted, and return the way to hand it back.
 *
 * Capture has to happen on mount: by the time the overlay is dismissed it holds focus itself, so
 * the opener is no longer discoverable. Restoration is deferred a frame because the overlay is
 * still unmounting when its cleanup runs, and it is skipped for an element that has since left the
 * document — focusing a detached node silently moves focus to `<body>`.
 *
 * The returned function is stable, so callers may use it as a hook dependency or as a cleanup.
 */
export function useRestorePreviousFocus(): () => void {
  const previousFocus = useRef(
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );
  return useCallback(() => {
    const previous = previousFocus.current;
    if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
  }, []);
}
