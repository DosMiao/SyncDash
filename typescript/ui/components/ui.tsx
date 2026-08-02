import {
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react';
import { createPortal } from 'react-dom';
import { Check } from 'lucide-react';
import { InteractionLayerScope, useInteractionLayer } from '../hooks/useInteractionLayer';
import { menuFocusIndex, type MenuNavigationKey } from './a11y';

interface MenuContextValue { close: () => void }

const MenuContext = createContext<MenuContextValue>({ close: () => {} });

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

function directMenuItems(panel: HTMLElement): HTMLElement[] {
  return [...panel.querySelectorAll<HTMLElement>('[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]')]
    .filter((item) => item.closest('[role="menu"]') === panel && !item.matches(':disabled, [aria-disabled="true"]'));
}

function moveMenuFocus(event: ReactKeyboardEvent<HTMLElement>, panel: HTMLElement): boolean {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return false;
  const menuItems = directMenuItems(panel);
  const currentIndex = menuItems.indexOf(document.activeElement as HTMLElement);
  const nextIndex = menuFocusIndex(
    event.key as MenuNavigationKey,
    currentIndex,
    menuItems.length,
  );
  if (nextIndex === null) return false;
  event.preventDefault();
  event.stopPropagation();
  menuItems[nextIndex]?.focus();
  return true;
}

/// The 6px inset keeps a clamped floating panel visually separate from the window frame.
export function clamp(
  left: number,
  top: number,
  width: number,
  height: number,
): { left: number; top: number } {
  return {
    left: Math.max(6, Math.min(left, window.innerWidth - width - 6)),
    top: Math.max(6, Math.min(top, window.innerHeight - height - 6)),
  };
}

function useOutsidePointerDismissal(
  open: boolean,
  close: () => void,
  inside: React.RefObject<HTMLElement | null>[],
) {
  const latestClose = useRef(close);
  latestClose.current = close;

  useEffect(() => {
    if (!open) return;
    const handleMouseDown = (event: MouseEvent) => {
      if (inside.some((insideRef) => insideRef.current?.contains(event.target as Node))) return;
      latestClose.current();
    };
    document.addEventListener('mousedown', handleMouseDown);
    return () => document.removeEventListener('mousedown', handleMouseDown);
    // `inside` is a fresh array literal each render; the refs inside it are stable, which is what
    // actually matters, so it is deliberately not a dependency
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
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

  useLayoutEffect(() => {
    const anchorElement = anchor.current;
    const panelElement = panel.current;
    if (!open || !anchorElement || !panelElement) return;
    const updatePosition = () => {
      const rect = anchorElement.getBoundingClientRect();
      const width = panelElement.offsetWidth || 200;
      const height = panelElement.offsetHeight || 200;
      const position = clamp(
        align === 'end' ? rect.right - width : rect.left,
        rect.bottom + 4,
        width,
        height,
      );
      panelElement.style.setProperty('--floating-panel-top', `${position.top}px`);
      panelElement.style.setProperty('--floating-panel-left', `${position.left}px`);
    };
    updatePosition();
    window.addEventListener('resize', updatePosition);
    document.addEventListener('scroll', updatePosition, true);
    return () => {
      window.removeEventListener('resize', updatePosition);
      document.removeEventListener('scroll', updatePosition, true);
      panelElement.style.removeProperty('--floating-panel-top');
      panelElement.style.removeProperty('--floating-panel-left');
    };
  }, [open, align]);

  useOutsidePointerDismissal(
    open,
    () => setOpen(false),
    [panel, anchor],
  );

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
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
      >
        {trigger}
      </button>
      {open ? (
        <InteractionLayerScope layerId={layerId}>
          <MenuContext.Provider value={{ close: () => setOpen(false) }}>
            {/* Off-screen until the first layout has measured the panel — otherwise it paints once
                at the unclamped position and visibly jumps */}
            <div
              id={panelId}
              ref={panel}
              className="menu-panel"
              role="dialog"
              aria-label={title ?? 'More information'}
              tabIndex={-1}
              onClick={(e) => e.stopPropagation()}
            >
              {children}
            </div>
          </MenuContext.Provider>
        </InteractionLayerScope>
      ) : null}
    </>
  );
}

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
      const position = clamp(at.x, at.y, panelElement.offsetWidth, panelElement.offsetHeight);
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

  // Scrolling the list underneath would leave the menu pointing at a row that has moved on
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
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => { moveMenuFocus(e, e.currentTarget); }}
        >
          {children}
        </div>
      </MenuContext.Provider>
    </InteractionLayerScope>
  );
}

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
    .filter((el) => el.getAttribute('aria-hidden') !== 'true');
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
    // mousedown preserves a drag that starts inside the sheet and releases on the scrim.
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
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key !== 'Tab' || !isTopLayer()) return;
          const focusable = panel.current ? focusableIn(panel.current) : [];
          if (focusable.length === 0) {
            e.preventDefault();
            panel.current?.focus();
            return;
          }
          const first = focusable[0]!;
          const last = focusable[focusable.length - 1]!;
          if (e.shiftKey && (document.activeElement === first || !panel.current?.contains(document.activeElement))) {
            e.preventDefault();
            last.focus();
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault();
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

export interface ConfirmAction {
  label: string;
  onConfirm: () => void;
  danger?: boolean;
  disabled?: boolean;
}

export function ConfirmDialog({ title, message, actions, onCancel }: {
  title: string;
  message: string;
  actions: ConfirmAction[];
  onCancel: () => void;
}) {
  const dangerConfirmation = actions.some((action) => action.danger && !action.disabled);
  const firstEnabledAction = actions.findIndex((action) => !action.disabled);
  return (
    <Sheet
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" autoFocus={dangerConfirmation} onClick={onCancel}>Cancel (Esc)</button>
          {actions.map((action, index) => (
            <button
              key={action.label}
              type="button"
              className={'btn ' + (action.danger ? 'danger' : 'accent')}
              disabled={action.disabled}
              autoFocus={!dangerConfirmation && index === firstEnabledAction}
              onClick={() => { onCancel(); action.onConfirm(); }}
            >
              {action.label}
            </button>
          ))}
        </>
      }
    >
      <div className="dialog-message">{message}</div>
    </Sheet>
  );
}

export function Placeholder({ icon, title, description }: {
  icon: ReactNode;
  title: string;
  description?: string;
}) {
  return (
    <div className="placeholder">
      <div className="placeholder-icon">{icon}</div>
      <div className="placeholder-title">{title}</div>
      {description ? <div className="placeholder-desc">{description}</div> : null}
    </div>
  );
}
