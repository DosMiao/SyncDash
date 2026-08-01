export type MenuNavigationKey = 'ArrowDown' | 'ArrowUp' | 'Home' | 'End';

export function menuFocusIndex(
  key: MenuNavigationKey,
  current: number,
  itemCount: number,
): number | null {
  if (itemCount === 0) return null;
  if (key === 'Home') return 0;
  if (key === 'End') return itemCount - 1;
  if (key === 'ArrowDown') return current < 0 ? 0 : (current + 1) % itemCount;
  return current < 0 ? itemCount - 1 : (current - 1 + itemCount) % itemCount;
}
