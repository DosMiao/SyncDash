import { useEffect, useLayoutEffect, useRef, type ReactNode } from 'react';
import { InteractionLayerScope, useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import { clampFloatingPanel } from '../floating/geometry.ts';
import { MenuContext } from './Menu.tsx';
import { directMenuItems, moveMenuFocus, useOutsidePointerDismissal } from './menuInteraction.ts';

export interface ContextPoint { x: number; y: number }

export function ContextMenu({ at, onClose, children }: {
  at: ContextPoint;
  onClose: () => void;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);
  const { layerId } = useInteractionLayer({
    kind: 'menu',
    rootRef: panel,
    handlers: { dismiss: onClose },
  });

  useLayoutEffect(() => {
    const panelElement = panel.current;
    if (!panelElement) return;
    const updatePosition = () => {
      const position = clampFloatingPanel(
        at.x,
        at.y,
        panelElement.offsetWidth,
        panelElement.offsetHeight,
      );
      panelElement.style.setProperty('--floating-panel-top', `${position.top}px`);
      panelElement.style.setProperty('--floating-panel-left', `${position.left}px`);
    };
    updatePosition();
    window.addEventListener('resize', updatePosition);
    return () => {
      window.removeEventListener('resize', updatePosition);
      panelElement.style.removeProperty('--floating-panel-top');
      panelElement.style.removeProperty('--floating-panel-left');
    };
  }, [at.x, at.y]);

  useOutsidePointerDismissal(true, onClose, [panel]);

  useLayoutEffect(() => {
    directMenuItems(panel.current!)[0]?.focus();
    return () => {
      const previous = previousFocus.current;
      if (previous?.isConnected) requestAnimationFrame(() => previous.focus());
    };
  }, []);

  // Scrolling the list underneath would leave the menu pointing at a row that has moved.
  useEffect(() => {
    document.addEventListener('scroll', onClose, true);
    return () => document.removeEventListener('scroll', onClose, true);
  }, [onClose]);

  return (
    <InteractionLayerScope layerId={layerId}>
      <MenuContext.Provider value={{ close: onClose }}>
        <div
          ref={panel}
          className="menu-panel"
          role="menu"
          aria-label="Row actions"
          onClick={(event) => event.stopPropagation()}
          onKeyDown={(event) => { moveMenuFocus(event, event.currentTarget); }}
        >
          {children}
        </div>
      </MenuContext.Provider>
    </InteractionLayerScope>
  );
}
