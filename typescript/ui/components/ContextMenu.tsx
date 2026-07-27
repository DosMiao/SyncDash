import { useLayoutEffect, useRef, useState } from 'react';

export interface CtxItem {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  sep?: boolean;
  run?: () => void;
}

export interface CtxState { x: number; y: number; items: CtxItem[] }

/// Positioned through the style prop, i.e. CSSOM — under the nonce CSP a style="" attribute is blocked.
/// Placement is corrected after the first layout, once the menu's real size is known.
export function ContextMenu({ state, onClose }: { state: CtxState; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: state.x, top: state.y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth, h = el.offsetHeight;
    // Lower bound 6: when the window is narrower than the menu, min() goes negative and the menu leaves
    // the screen entirely
    setPos({
      left: Math.max(6, Math.min(state.x, window.innerWidth - w - 6)),
      top: Math.max(6, Math.min(state.y, window.innerHeight - h - 6)),
    });
  }, [state]);

  return (
    <div id="ctxmenu" ref={ref} style={pos} onClick={(e) => e.stopPropagation()}>
      {state.items.map((it, k) =>
        it.sep ? (
          <div key={k} className="ctxsep" />
        ) : (
          <div
            key={k}
            className={'ctxitem' + (it.disabled ? ' off' : '') + (it.danger ? ' danger' : '')}
            onClick={() => { if (it.disabled || !it.run) return; onClose(); it.run(); }}
          >{it.label}</div>
        ))}
    </div>
  );
}
