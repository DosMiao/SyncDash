export type RootEditKeyAction = 'commit' | 'revert' | null;

export function rootEditKeyAction(key: string): RootEditKeyAction {
  if (key === 'Enter') return 'commit';
  if (key === 'Escape') return 'revert';
  return null;
}
