import { useCallback, useEffect, useRef, useState } from 'react';

// The element is passed directly because a ref object's stable identity cannot signal replacement.

/// Height of the sticky section heading. The activation line sits just under it, so a section
/// becomes current when its *heading* reaches the top of the pane, not when its first row does.
/// Tracks .ed-section-title's rendered height in styles.css.
const STICKY_H = 30;

/// Ceiling on how long the rail stays pinned to a clicked section during the smooth scroll, for the
/// case where the pane never reaches the exact target (a short last section cannot scroll far
/// enough). Released as soon as it does land, so this is not a delay in the normal case.
const SMOOTH_SCROLL_HOLD_MS = 600;

export interface ScrollSpy {
  /// The section in view, or the target while smooth scrolling is in progress.
  active: string;
  /// Callback ref for a section element. Stable per ID: an inline callback would be a new
  /// function every render, and React responds to a changed ref identity by detaching with null
  /// and reattaching — so every render would rebuild the map this hook reads on every scroll frame.
  register: (sectionId: string) => (element: HTMLElement | null) => void;
  scrollTo: (sectionId: string, smooth?: boolean) => void;
}

/// `sectionIds` must be referentially stable; a fresh array re-subscribes the scroll listener.
export function useScrollSpy(scrollPane: HTMLElement | null, sectionIds: string[]): ScrollSpy {
  const [activeSectionId, setActiveSectionId] = useState(sectionIds[0] ?? '');
  const sectionElementsRef = useRef(new Map<string, HTMLElement>());
  const registrationCallbacksRef = useRef(
    new Map<string, (element: HTMLElement | null) => void>(),
  );
  const activeSectionHoldRef = useRef<{ sectionId: string; expiresAt: number } | null>(null);

  const register = useCallback((sectionId: string) => {
    let registrationCallback = registrationCallbacksRef.current.get(sectionId);
    if (!registrationCallback) {
      registrationCallback = (element: HTMLElement | null) => {
        if (element) sectionElementsRef.current.set(sectionId, element);
        else sectionElementsRef.current.delete(sectionId);
      };
      registrationCallbacksRef.current.set(sectionId, registrationCallback);
    }
    return registrationCallback;
  }, []);

  const measureActiveSection = useCallback(() => {
    if (!scrollPane || sectionIds.length === 0) return;

    const activeSectionHold = activeSectionHoldRef.current;
    if (activeSectionHold) {
      const targetSection = sectionElementsRef.current.get(activeSectionHold.sectionId);
      const targetScrollTop = targetSection
        ? Math.max(0, targetSection.offsetTop - STICKY_H)
        : null;
      const targetReached = targetScrollTop !== null
        && Math.abs(scrollPane.scrollTop - targetScrollTop) < 2;
      if (targetReached || performance.now() > activeSectionHold.expiresAt) {
        activeSectionHoldRef.current = null;
      } else {
        setActiveSectionId(activeSectionHold.sectionId);
        return;
      }
    }

    // The last section is the shortest and its top can never reach the activation line, so without
    // this it would never highlight however far you scrolled.
    if (scrollPane.scrollTop + scrollPane.clientHeight >= scrollPane.scrollHeight - 2) {
      setActiveSectionId(sectionIds[sectionIds.length - 1]);
      return;
    }

    // offsetTop is pane-relative only because .ed-pane is positioned — see styles.css
    const activationLine = scrollPane.scrollTop + STICKY_H + 8;
    let measuredActiveSectionId = sectionIds[0];
    for (const sectionId of sectionIds) {
      const sectionElement = sectionElementsRef.current.get(sectionId);
      if (sectionElement && sectionElement.offsetTop <= activationLine) {
        measuredActiveSectionId = sectionId;
      }
    }
    setActiveSectionId(measuredActiveSectionId);
  }, [scrollPane, sectionIds]);

  useEffect(() => {
    if (!scrollPane) return;
    let pendingAnimationFrameId: number | null = null;
    const handleScroll = () => {
      if (pendingAnimationFrameId !== null) return;
      pendingAnimationFrameId = requestAnimationFrame(() => {
        pendingAnimationFrameId = null;
        measureActiveSection();
      });
    };
    scrollPane.addEventListener('scroll', handleScroll, { passive: true });
    measureActiveSection();
    return () => {
      scrollPane.removeEventListener('scroll', handleScroll);
      if (pendingAnimationFrameId !== null) cancelAnimationFrame(pendingAnimationFrameId);
    };
  }, [measureActiveSection, scrollPane]);

  // Pinning `active` stops the rail strobing through intermediate sections during smooth scrolling.
  const scrollTo = useCallback((sectionId: string, smooth = true) => {
    const sectionElement = sectionElementsRef.current.get(sectionId);
    if (!scrollPane || !sectionElement) return;
    activeSectionHoldRef.current = {
      sectionId,
      expiresAt: performance.now() + SMOOTH_SCROLL_HOLD_MS,
    };
    setActiveSectionId(sectionId);
    scrollPane.scrollTo({
      top: Math.max(0, sectionElement.offsetTop - STICKY_H),
      behavior: smooth ? 'smooth' : 'auto',
    });
  }, [scrollPane]);

  return { active: activeSectionId, register, scrollTo };
}
