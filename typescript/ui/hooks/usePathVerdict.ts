import { useEffect, useState } from 'react';
import { inspectPaths } from '../../core/ipc';
import type { PathInfo } from '../../core/types/generated/PathInfo';
import type { PathVerdict } from '../../core/types/generated/PathVerdict';

/// Path health check (debounced): does it exist, is it a directory, are the two roots identical/nested.
/// Getting a root wrong is too costly to discover only at Compare time. The main path row and the job
/// editor share this one hook — both verdicts must say the same thing about the same pair of roots.
export function usePathVerdict(source: string, target: string, enabled = true): PathVerdict | null {
  const [verdict, setVerdict] = useState<PathVerdict | null>(null);

  useEffect(() => {
    if (!enabled || (!source && !target)) { setVerdict(null); return; }
    let live = true;
    const t = setTimeout(() => {
      inspectPaths(source, target)
        .then((v) => { if (live) setVerdict(v); })
        .catch(() => { /* a failed check must not block editing */ });
    }, 300);
    return () => { live = false; clearTimeout(t); };
  }, [source, target, enabled]);

  return verdict;
}

/// '' | 'good' | 'bad' for a root input, from the verdict for that side
export function pathState(info: PathInfo | undefined, value: string): string {
  if (!info) return '';
  if (info.is_dir) return 'good';
  return value.trim() ? 'bad' : '';
}
