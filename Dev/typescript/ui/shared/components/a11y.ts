const ROVING_FOCUS_KEYS = ['ArrowDown', 'ArrowRight', 'ArrowUp', 'ArrowLeft', 'Home', 'End'] as const;

export type RovingFocusKey = typeof ROVING_FOCUS_KEYS[number];

export function isRovingFocusKey(key: string): key is RovingFocusKey {
  return (ROVING_FOCUS_KEYS as readonly string[]).includes(key);
}

/// Wrap-around focus index for one roving-focus widget: menus, the job list, and anything else
/// whose arrow keys walk a single ring of items. `null` means the key moves nothing.
///
/// Both axes are accepted so no caller re-derives the modulo. Which keys a widget consumes is still
/// the widget's decision — a vertical menu deliberately ignores ArrowLeft/ArrowRight because those
/// belong to submenu traversal — but the arithmetic has one owner.
export function rovingFocusIndex(
  key: RovingFocusKey,
  current: number,
  itemCount: number,
): number | null {
  if (itemCount === 0) return null;
  if (key === 'Home') return 0;
  if (key === 'End') return itemCount - 1;
  if (key === 'ArrowDown' || key === 'ArrowRight') return current < 0 ? 0 : (current + 1) % itemCount;
  return current < 0 ? itemCount - 1 : (current - 1 + itemCount) % itemCount;
}
