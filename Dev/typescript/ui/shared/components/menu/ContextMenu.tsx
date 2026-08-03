import { useEffect, useLayoutEffect, useRef, type ReactNode } from 'react';
import { InteractionLayerScope, useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import { useRestorePreviousFocus } from '#ui/shared/hooks/useRestorePreviousFocus.ts';
import { useFloatingPosition } from '../floating/useFloatingPosition.ts';
import { MenuContext } from './Menu.tsx';
import { directMenuItems, moveMenuFocus, useOutsidePointerDismissal } from './menuInteraction.ts';

interface ContextPoint { x: number; y: number }

export function ContextMenu({ at, onClose, children }: {
  at: ContextPoint;
  onClose: () => void;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const restorePreviousFocus = useRestorePreviousFocus();
  const { layerId } = useInteractionLayer({
    kind: 'menu',
    rootRef: panel,
    handlers: { dismiss: onClose },
  });

  // Anchored to the pointer, not to an element: scrolling closes this menu instead of moving it.
  useFloatingPosition(
    panel,
    (panelElement) => ({
      left: at.x,
      top: at.y,
      width: panelElement.offsetWidth,
      height: panelElement.offsetHeight,
    }),
    { deps: [at.x, at.y] },
  );

  useOutsidePointerDismissal(true, onClose, [panel]);

  useLayoutEffect(() => {
    directMenuItems(panel.current!)[0]?.focus();
    return restorePreviousFocus;
  }, [restorePreviousFocus]);

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
