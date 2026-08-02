import { useEffect, useRef, useState } from 'react';
import { endpointInputState } from '../../core/endpoint-readiness';
import { inspectPaths } from '../../core/ipc';
import type { PathVerdict } from '../../core/types/generated/PathVerdict';

export type PathInspectionState =
  | { status: 'inactive' }
  | { status: 'debouncing'; requestId: number }
  | { status: 'checking'; requestId: number }
  | { status: 'ready'; requestId: number; verdict: PathVerdict }
  | { status: 'failed'; requestId: number; error: string };

export function usePathVerdict(source: string, target: string, enabled = true): PathInspectionState {
  const requestSequence = useRef(0);
  const [inspection, setInspection] = useState<PathInspectionState>({ status: 'inactive' });

  useEffect(() => {
    if (!enabled || (!source && !target)) {
      setInspection({ status: 'inactive' });
      return;
    }
    const requestId = requestSequence.current + 1;
    requestSequence.current = requestId;
    setInspection({ status: 'debouncing', requestId });
    let active = true;
    const timer = setTimeout(() => {
      if (!active || requestSequence.current !== requestId) return;
      setInspection({ status: 'checking', requestId });
      inspectPaths(source, target)
        .then((verdict) => {
          if (active && requestSequence.current === requestId) {
            setInspection({ status: 'ready', requestId, verdict });
          }
        })
        .catch((error) => {
          if (active && requestSequence.current === requestId) {
            setInspection({ status: 'failed', requestId, error: String(error) });
          }
        });
    }, 300);
    return () => { active = false; clearTimeout(timer); };
  }, [source, target, enabled]);

  return inspection;
}

export function pathState(
  inspection: PathInspectionState,
  field: 'source' | 'target',
  value: string,
): string {
  return endpointInputState(inspection.status === 'ready' ? inspection.verdict[field] : undefined, value);
}
