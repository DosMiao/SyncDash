import { useCallback, useEffect, useRef, useState } from 'react';

// A table of contents that follows the page, for a rail beside one long scrolling pane.
//
// The pane arrives as an element rather than a ref for the reason spelled out on useVirtualRows.

/// Height of the sticky section heading. The activation line sits just under it, so a section
/// becomes current when its *heading* reaches the top of the pane, not when its first row does.
/// Tracks .ed-section-title's rendered height in styles.css.
const STICKY_H = 30;

/// Ceiling on how long the rail stays pinned to a clicked section during the smooth scroll, for the
/// case where the pane never reaches the exact target (a short last section cannot scroll far
/// enough). Released as soon as it does land, so this is not a delay in the normal case.
const GLIDE_MS = 600;

export interface ScrollSpy {
  /// The section in view, or the clicked one while a glide is still running
  active: string;
  /// Callback ref for a section element. Stable per id: an inline `(el) => …` would be a new
  /// function every render, and React responds to a changed ref identity by detaching with null
  /// and reattaching — so every render would rebuild the map this hook reads on every scroll frame.
  register: (id: string) => (el: HTMLElement | null) => void;
  scrollTo: (id: string, smooth?: boolean) => void;
}

/// `ids` must be referentially stable — a fresh array each render re-subscribes the scroll listener.
export function useScrollSpy(pane: HTMLElement | null, ids: string[]): ScrollSpy {
  const [active, setActive] = useState(ids[0] ?? '');
  const els = useRef(new Map<string, HTMLElement>());
  const refs = useRef(new Map<string, (el: HTMLElement | null) => void>());
  const glide = useRef<{ id: string; until: number } | null>(null);

  const register = useCallback((id: string) => {
    let cb = refs.current.get(id);
    if (!cb) {
      cb = (el: HTMLElement | null) => {
        if (el) els.current.set(id, el);
        else els.current.delete(id);
      };
      refs.current.set(id, cb);
    }
    return cb;
  }, []);

  const measure = useCallback(() => {
    if (!pane || ids.length === 0) return;

    const held = glide.current;
    if (held) {
      const target = els.current.get(held.id);
      const landed = target && Math.abs(pane.scrollTop - Math.max(0, target.offsetTop - STICKY_H)) < 2;
      if (landed || performance.now() > held.until) glide.current = null;
      else { setActive(held.id); return; }
    }

    // The last section is the shortest and its top can never reach the activation line, so without
    // this it would never highlight however far you scrolled.
    if (pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 2) {
      setActive(ids[ids.length - 1]);
      return;
    }

    // offsetTop is pane-relative only because .ed-pane is positioned — see styles.css
    const line = pane.scrollTop + STICKY_H + 8;
    let hit = ids[0];
    for (const id of ids) {
      const el = els.current.get(id);
      if (el && el.offsetTop <= line) hit = id;
    }
    setActive(hit);
  }, [pane, ids]);

  useEffect(() => {
    if (!pane) return;
    let pending = false;
    const onScroll = () => {
      if (pending) return;
      pending = true;
      requestAnimationFrame(() => { pending = false; measure(); });
    };
    pane.addEventListener('scroll', onScroll, { passive: true });
    measure();
    return () => pane.removeEventListener('scroll', onScroll);
  }, [pane, measure]);

  // Pinning `active` for the duration is what stops the rail strobing through every section the
  // glide passes over.
  const scrollTo = useCallback((id: string, smooth = true) => {
    const el = els.current.get(id);
    if (!pane || !el) return;
    glide.current = { id, until: performance.now() + GLIDE_MS };
    setActive(id);
    pane.scrollTo({ top: Math.max(0, el.offsetTop - STICKY_H), behavior: smooth ? 'smooth' : 'auto' });
  }, [pane]);

  return { active, register, scrollTo };
}
