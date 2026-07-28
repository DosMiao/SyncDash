// The UI's parts: menus, context menus, confirmations, modal sheets, empty states.
//
// Native controls such as menus, confirmation dialogs and tooltips have no equivalent inside a
// WebView, and this file supplies one of each. The rules:
//   · Menus close on an outside click or Esc, and clamp themselves back inside the viewport
//   · A confirmation spells out what pressing the button will do, rather than merely asking
//     "are you sure" — window.confirm can do neither, and looks nothing like the rest of the app
//   · One modal shell, three widths. Three hand-rolled overlays was how the old #modal /
//     #editmodal / #setmodal ended up with three different z-indexes and two different paddings
//
// Every export here has a caller. Anything that stops having one gets deleted rather than kept
// "for symmetry with GitDash" — an unused primitive is a decision nobody made.

import {
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { Check, ChevronRight } from 'lucide-react';

// ----------------------------------------------------------------- menus --

interface MenuContext { close: () => void }

const MenuCtx = createContext<MenuContext>({ close: () => {} });

export function MenuItem({ children, onClick, disabled, checked, title, danger }: {
  children: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  checked?: boolean;
  title?: string;
  danger?: boolean;
}) {
  const { close } = useContext(MenuCtx);
  return (
    <button
      type="button"
      className={'menu-item' + (danger ? ' menu-item-danger' : '')}
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

export function MenuSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <>
      <div className="menu-section">{title}</div>
      {children}
    </>
  );
}

/// A submenu. Opens on hover or click, and its contents are a plain layer of MenuItems.
export function SubMenu({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Asymmetric delays: opening should feel immediate, but closing waits long enough to cross the
  // diagonal gap between the parent item and the panel without the menu vanishing mid-travel
  const schedule = (next: boolean) => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(next), next ? 60 : 160);
  };

  useEffect(() => () => { if (timer.current) clearTimeout(timer.current); }, []);

  return (
    <div className="submenu" onMouseEnter={() => schedule(true)} onMouseLeave={() => schedule(false)}>
      <button type="button" className="menu-item" onClick={() => setOpen((v) => !v)}>
        <span className="menu-check" />
        <span className="menu-label">{label}</span>
        <ChevronRight size={12} className="menu-arrow" />
      </button>
      {open ? <div className="menu-panel submenu-panel">{children}</div> : null}
    </div>
  );
}

/// Clamp a panel of the given size back inside the viewport. 6px so a panel against an edge still
/// reads as floating rather than welded to the window frame. Exported because every floating layer
/// needs it and three copies of this arithmetic is how one of them ends up clamping only sideways.
export function clamp(x: number, y: number, w: number, h: number): { left: number; top: number } {
  return {
    left: Math.max(6, Math.min(x, window.innerWidth - w - 6)),
    top: Math.max(6, Math.min(y, window.innerHeight - h - 6)),
  };
}

/**
 * Escape closes the **topmost** dismissible thing and nothing else.
 *
 * Every layer listens on `document`, so without a stack one Escape would close the context menu,
 * the sheet it sits in, and the sheet behind that, all at once — the job editor's delete
 * confirmation is genuinely nested inside the editor, so this is not hypothetical. Registration
 * order is open order, so the last entry is the top layer.
 */
const escStack: symbol[] = [];

function useEscape(active: boolean, close: () => void) {
  // Through a ref so the effect's deps stay empty: re-running it on every render would pop and
  // re-push this layer, and a re-render of the layer *underneath* would then put it on top
  const latest = useRef(close);
  latest.current = close;

  useEffect(() => {
    if (!active) return;
    const id = Symbol('esc');
    escStack.push(id);
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || escStack[escStack.length - 1] !== id) return;
      e.stopPropagation();
      latest.current();
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      const k = escStack.indexOf(id);
      if (k >= 0) escStack.splice(k, 1);
    };
  }, [active]);
}

/// Close on an outside click or Esc. Shared by Menu and ContextMenu — the two differ only in
/// what opens them.
function useDismiss(open: boolean, close: () => void, inside: React.RefObject<HTMLElement | null>[]) {
  const latest = useRef(close);
  latest.current = close;

  useEscape(open, close);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (inside.some((r) => r.current?.contains(e.target as Node))) return;
      latest.current();
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
    // `inside` is a fresh array literal each render; the refs inside it are stable, which is what
    // actually matters, so it is deliberately not a dependency
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
}

/**
 * A dropdown menu. `trigger` decides how it looks and `children` are its contents.
 *
 * The panel is fixed-positioned and clamped back inside the viewport after render — a control
 * near the bottom-right of the window that simply expanded down and to the right would end up
 * half off-screen.
 */
export function Menu({ trigger, children, disabled, title, align = 'start', className }: {
  trigger: ReactNode;
  children: ReactNode;
  disabled?: boolean;
  title?: string;
  align?: 'start' | 'end';
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const anchor = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (!open || !anchor.current) return;
    const rect = anchor.current.getBoundingClientRect();
    const w = panel.current?.offsetWidth ?? 200;
    const h = panel.current?.offsetHeight ?? 200;
    setPos(clamp(align === 'end' ? rect.right - w : rect.left, rect.bottom + 4, w, h));
  }, [open, align]);

  useDismiss(open, () => setOpen(false), [panel, anchor]);

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={className}
        title={title}
        disabled={disabled}
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
      >
        {trigger}
      </button>
      {open ? (
        <MenuCtx.Provider value={{ close: () => setOpen(false) }}>
          {/* Off-screen until the first layout has measured the panel — otherwise it paints once
              at the unclamped position and visibly jumps */}
          <div
            ref={panel}
            className="menu-panel"
            style={{ top: pos?.top ?? -9999, left: pos?.left ?? -9999 }}
            onClick={(e) => e.stopPropagation()}
          >
            {children}
          </div>
        </MenuCtx.Provider>
      ) : null}
    </>
  );
}

export interface ContextPoint { x: number; y: number }

/// A context menu opened at a point. Its contents are the same layer of MenuItems a Menu takes,
/// so a row's right-click menu and a toolbar dropdown cannot drift apart in look or behaviour.
export function ContextMenu({ at, onClose, children }: {
  at: ContextPoint;
  onClose: () => void;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number }>({ left: at.x, top: at.y });

  useLayoutEffect(() => {
    const el = panel.current;
    if (!el) return;
    setPos(clamp(at.x, at.y, el.offsetWidth, el.offsetHeight));
  }, [at]);

  useDismiss(true, onClose, [panel]);

  // Scrolling the list underneath would leave the menu pointing at a row that has moved on
  useEffect(() => {
    document.addEventListener('scroll', onClose, true);
    return () => document.removeEventListener('scroll', onClose, true);
  }, [onClose]);

  return (
    <MenuCtx.Provider value={{ close: onClose }}>
      <div ref={panel} className="menu-panel" style={pos} onClick={(e) => e.stopPropagation()}>
        {children}
      </div>
    </MenuCtx.Provider>
  );
}

// ---------------------------------------------------------------- sheets --

/**
 * The modal shell: scrim, panel, title, scrolling body, button row.
 *
 * `width` is the only thing the three callers disagree about — a confirmation is a column of
 * numbers, the settings form is two columns, the job editor is two columns of longer fields.
 */
export function Sheet({ title, width = 'sm', children, footer, onClose }: {
  title: string;
  width?: 'sm' | 'mid' | 'wide' | 'xl';
  children: ReactNode;
  footer: ReactNode;
  onClose: () => void;
}) {
  const labelId = useId();
  useEscape(true, onClose);

  return (
    // mousedown, not click: a drag that starts inside the sheet and releases on the scrim would
    // otherwise close it, which is how a text selection in the body used to dismiss the dialog
    <div className="scrim" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div
        className={'sheet' + (width === 'sm' ? '' : ` sheet-${width}`)}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelId}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <h3 id={labelId}>{title}</h3>
        <div className="sheet-body">{children}</div>
        <div className="btnrow">{footer}</div>
      </div>
    </div>
  );
}

export interface ConfirmAction {
  label: string;
  onConfirm: () => void;
  danger?: boolean;
  disabled?: boolean;
}

/**
 * A confirmation. The title is a question and the body spells out in full what pressing the
 * button will do.
 *
 * The body renders as pre-wrap: these explanations carry path lists, one per line, and squashing
 * them into a paragraph is the same as not writing them.
 */
export function ConfirmDialog({ title, message, actions, onCancel }: {
  title: string;
  message: string;
  actions: ConfirmAction[];
  onCancel: () => void;
}) {
  return (
    <Sheet
      title={title}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>Cancel (Esc)</button>
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              className={'btn ' + (a.danger ? 'danger' : 'accent')}
              disabled={a.disabled}
              autoFocus={!a.disabled}
              onClick={() => { onCancel(); a.onConfirm(); }}
            >
              {a.label}
            </button>
          ))}
        </>
      }
    >
      <div className="dialog-message">{message}</div>
    </Sheet>
  );
}

// --------------------------------------------------------- empty states --

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
