import type { PathInfo } from './types/generated/PathInfo';

/// Input-border state derived only from facts the endpoint probe actually observed. Network roots
/// stay neutral while deferred; painting them green would claim the editor connected when it did not.
export function endpointInputState(info: PathInfo | undefined, value: string): '' | 'good' | 'bad' {
  if (!info || !value.trim()) return '';
  switch (info.readiness) {
    case 'ready': return 'good';
    case 'missing':
    case 'not_directory':
    case 'invalid': return 'bad';
    case 'empty':
    case 'deferred':
    case 'unobservable': return '';
  }
}
