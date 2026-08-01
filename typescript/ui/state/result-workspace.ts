import type { CompareOwner } from '../../core/types/generated/CompareOwner';

export type ResultView = 'differences' | 'identical';

interface PreferenceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const RUN_SCOPE_PANEL_KEY = 'sd.scope';
const LEGACY_OVERVIEW_PANEL_KEY = 'sd.ov';

export function readRunScopePanelCollapsed(storage: PreferenceStorage): boolean {
  const current = storage.getItem(RUN_SCOPE_PANEL_KEY);
  if (current !== null) return current !== 'open';
  const legacy = storage.getItem(LEGACY_OVERVIEW_PANEL_KEY);
  if (legacy === null) return true;
  storage.setItem(RUN_SCOPE_PANEL_KEY, legacy);
  storage.removeItem(LEGACY_OVERVIEW_PANEL_KEY);
  return legacy !== 'open';
}

export function writeRunScopePanelCollapsed(storage: PreferenceStorage, collapsed: boolean): void {
  storage.setItem(RUN_SCOPE_PANEL_KEY, collapsed ? 'closed' : 'open');
}

export function identicalResultRequestKey(
  owner: CompareOwner,
  query: string,
  offset: number,
): string {
  // job_name is a mutable display label; the remaining owner fields are the evidence identity.
  return [
    owner.compare_id,
    owner.job_id,
    owner.target_index,
    owner.config_revision,
    query,
    offset,
  ].join('\0');
}

export interface ApplyAvailability {
  available: boolean;
  blockedMessage: string | null;
}

export function deriveApplyAvailability(input: {
  hasPlan: boolean;
  resultView: ResultView;
  scopeCalculationPending: boolean;
  scopeCalculationFailed: boolean;
  executableCount: number;
}): ApplyAvailability {
  if (!input.hasPlan) {
    return {
      available: false,
      blockedMessage: 'Compare first to create a result that can be reviewed and applied',
    };
  }
  if (input.resultView !== 'differences') {
    return {
      available: false,
      blockedMessage: 'Switch to Differences to review the run scope before applying it',
    };
  }
  if (input.scopeCalculationFailed) {
    return {
      available: false,
      blockedMessage: 'The run scope could not be calculated safely; clear or revise the failed filter',
    };
  }
  if (input.scopeCalculationPending) {
    return {
      available: false,
      blockedMessage: 'The run scope is still being calculated; wait a moment and try again',
    };
  }
  if (input.executableCount === 0) {
    return {
      available: false,
      blockedMessage: 'No checked differences are currently in the run scope',
    };
  }
  return { available: true, blockedMessage: null };
}
