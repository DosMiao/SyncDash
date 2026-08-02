import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from 'react';
import { menuFocusIndex, type MenuNavigationKey } from '../a11y.ts';

export function directMenuItems(panel: HTMLElement): HTMLElement[] {
  return [...panel.querySelectorAll<HTMLElement>(
    '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]',
  )].filter((item) => (
    item.closest('[role="menu"]') === panel
    && !item.matches(':disabled, [aria-disabled="true"]')
  ));
}

export function moveMenuFocus(
  event: ReactKeyboardEvent<HTMLElement>,
  panel: HTMLElement,
): boolean {
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

export function useOutsidePointerDismissal(
  open: boolean,
  close: () => void,
  inside: RefObject<HTMLElement | null>[],
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
    // actually matters, so it is deliberately not a dependency.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
}
