import {
  createContext,
  useContext,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { Check } from 'lucide-react';
import { InteractionLayerScope, useInteractionLayer } from '#ui/shared/interaction/useInteractionLayer.tsx';
import { useFloatingPosition } from '../floating/useFloatingPosition.ts';
import { useOutsidePointerDismissal } from './menuInteraction.ts';

interface MenuContextValue { close: () => void }

export const MenuContext = createContext<MenuContextValue>({ close: () => {} });

export function MenuItem({ children, onClick, disabled, checked, title, danger }: {
  children: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  checked?: boolean;
  title?: string;
  danger?: boolean;
}) {
  const { close } = useContext(MenuContext);
  return (
    <button
      type="button"
      className={'menu-item' + (danger ? ' menu-item-danger' : '')}
      role={checked === undefined ? 'menuitem' : 'menuitemcheckbox'}
      aria-checked={checked === undefined ? undefined : checked}
      tabIndex={-1}
      disabled={disabled}
      title={title}
      onClick={() => { close(); onClick?.(); }}
    >
      <span className="menu-check">{checked ? <Check size={12} /> : null}</span>
      <span className="menu-label">{children}</span>
    </button>
  );
}

export function MenuDivider() {
  return <div className="menu-divider" role="separator" />;
}

export function Menu({ trigger, children, disabled, title, align = 'start', className }: {
  trigger: ReactNode;
  children: ReactNode;
  disabled?: boolean;
  title?: string;
  align?: 'start' | 'end';
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const panelId = useId();
  const { layerId } = useInteractionLayer({
    active: open,
    kind: 'menu',
    rootRef: panel,
    handlers: {
      dismiss: () => {
        setOpen(false);
        requestAnimationFrame(() => anchor.current?.focus());
      },
    },
  });

  // The anchor is re-measured on every reposition rather than captured once: a scroll or a resize
  // moves the trigger, and the panel has to stay attached to it.
  useFloatingPosition(
    panel,
    (panelElement) => {
      const anchorElement = anchor.current;
      if (!anchorElement) return null;
      const rect = anchorElement.getBoundingClientRect();
      const width = panelElement.offsetWidth || 200;
      const height = panelElement.offsetHeight || 200;
      return {
        left: align === 'end' ? rect.right - width : rect.left,
        top: rect.bottom + 4,
        width,
        height,
      };
    },
    { enabled: open, repositionOnScroll: true, deps: [open, align] },
  );

  useOutsidePointerDismissal(open, () => setOpen(false), [panel, anchor]);

  useLayoutEffect(() => {
    if (open) panel.current?.focus();
  }, [open]);

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={className}
        title={title}
        aria-label={title}
        disabled={disabled}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        onClick={(event) => { event.stopPropagation(); setOpen((value) => !value); }}
      >
        {trigger}
      </button>
      {open ? (
        <InteractionLayerScope layerId={layerId}>
          <MenuContext.Provider value={{ close: () => setOpen(false) }}>
            {/* Off-screen until the first layout has measured the panel; this prevents a flash at
                the unclamped position. */}
            <div
              id={panelId}
              ref={panel}
              className="menu-panel"
              role="dialog"
              aria-label={title ?? 'More information'}
              tabIndex={-1}
              onClick={(event) => event.stopPropagation()}
            >
              {children}
            </div>
          </MenuContext.Provider>
        </InteractionLayerScope>
      ) : null}
    </>
  );
}
