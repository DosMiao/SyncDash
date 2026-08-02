import { useId, useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { InteractionLayerScope, useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';

const FOCUSABLE = [
  'button:not(:disabled)',
  'input:not(:disabled):not([type="hidden"])',
  'select:not(:disabled)',
  'textarea:not(:disabled)',
  'a[href]',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

function focusableIn(panel: HTMLElement): HTMLElement[] {
  return [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)]
    .filter((element) => element.getAttribute('aria-hidden') !== 'true');
}

export function Sheet({ title, width = 'sm', children, footer, onClose }: {
  title: string;
  width?: 'sm' | 'mid' | 'wide' | 'xl';
  children: ReactNode;
  footer: ReactNode;
  onClose: () => void;
}) {
  const labelId = useId();
  const scrim = useRef<HTMLDivElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const { layerId, isTopLayer } = useInteractionLayer({
    kind: 'modal',
    rootRef: scrim,
    handlers: { dismiss: onClose },
  });

  useLayoutEffect(() => {
    const frame = requestAnimationFrame(() => {
      if (!isTopLayer()) return;
      const target = panel.current?.querySelector<HTMLElement>('[autofocus]')
        ?? (panel.current ? focusableIn(panel.current)[0] : null)
        ?? panel.current;
      target?.focus();
    });

    return () => {
      cancelAnimationFrame(frame);
      const previous = previousFocus.current;
      if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
    };
  }, [isTopLayer]);

  const dialog = (
    // Mousedown preserves a drag that starts inside the sheet and releases on the scrim.
    <div
      ref={scrim}
      className="scrim"
      onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}
    >
      <div
        ref={panel}
        className={'sheet' + (width === 'sm' ? '' : ` sheet-${width}`)}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelId}
        tabIndex={-1}
        onMouseDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key !== 'Tab' || !isTopLayer()) return;
          const focusable = panel.current ? focusableIn(panel.current) : [];
          if (focusable.length === 0) {
            event.preventDefault();
            panel.current?.focus();
            return;
          }
          const first = focusable[0]!;
          const last = focusable[focusable.length - 1]!;
          if (event.shiftKey && (document.activeElement === first || !panel.current?.contains(document.activeElement))) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <h3 id={labelId}>{title}</h3>
        <div className="sheet-body">{children}</div>
        <div className="btnrow">{footer}</div>
      </div>
    </div>
  );

  return createPortal(
    <InteractionLayerScope layerId={layerId}>{dialog}</InteractionLayerScope>,
    document.body,
  );
}
