import { useCallback, useEffect, useState } from 'react';
import { applyZoom, readZoom, stepZoom } from '../../core/zoom';

/// Ctrl/Cmd +, -, 0 plus a status-bar stepper. Registered once at the app root so the shortcuts work
/// wherever focus is — including inside the diff table, which is where you actually notice the type size.
export function useZoomControl() {
  const [zoom, setZoom] = useState(readZoom);

  // Re-assert on mount: setZoom lives on the webview, not in the document, so a reload starts back at 1
  useEffect(() => { void applyZoom(readZoom()).then(setZoom); }, []);

  const change = useCallback((next: number) => { void applyZoom(next).then(setZoom); }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      // '=' is the unshifted key on the '+' cap, and the numpad reports 'Add'/'Subtract' as +/-
      if (e.key === '+' || e.key === '=') { e.preventDefault(); change(stepZoom(readZoom(), 1)); }
      else if (e.key === '-' || e.key === '_') { e.preventDefault(); change(stepZoom(readZoom(), -1)); }
      else if (e.key === '0') { e.preventDefault(); change(1); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [change]);

  return {
    zoom,
    zoomIn: () => change(stepZoom(zoom, 1)),
    zoomOut: () => change(stepZoom(zoom, -1)),
    zoomReset: () => change(1),
  };
}
