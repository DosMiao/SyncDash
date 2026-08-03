import { useCallback, useRef, useState } from 'react';
import type { Dispatch } from 'react';
import { resolveCompareResultForget } from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import type { CompareForgetRequest } from '#core/application/compare-workspace/compareWorkspaceForget.ts';
import type {
  CompareResultKey,
  CompareWorkspaceRepository,
} from '#core/application/compare-workspace/compareWorkspaceModel.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import * as compareIpc from '#core/infrastructure/tauri/commands/compare.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

interface ForgetCompareResultOptions {
  repository: CompareWorkspaceRepository;
  runInFlight: boolean;
  reviewPending: boolean;
  dispatch: Dispatch<CompareWorkspaceAction>;
  resetSafetyUi: () => void;
  setStatus: StatusApi['setMessage'];
}

/**
 * Discards one exact Compare result permanently. The confirmation is re-answered here rather than
 * trusted from the moment the dialog opened, and the workspace only drops the result once the
 * backend has actually forgotten it — a failed forget leaves the evidence exactly where it was.
 */
export function useForgetCompareResult({
  repository,
  runInFlight,
  reviewPending,
  dispatch,
  resetSafetyUi,
  setStatus,
}: ForgetCompareResultOptions) {
  const inFlight = useRef<CompareResultKey | null>(null);
  const [pending, setPending] = useState(false);

  const forgetResult = useCallback(async (request: CompareForgetRequest) => {
    if (inFlight.current !== null) return;
    const resolution = resolveCompareResultForget(repository, request, { runInFlight, reviewPending });
    if (resolution.status === 'refused') {
      setStatus(resolution.message, 'err');
      return;
    }

    inFlight.current = request.resultKey;
    setPending(true);
    try {
      const outcome = await compareIpc.forgetCompareResult(resolution.identity);
      dispatch({ type: 'result_forgotten', resultKey: request.resultKey });
      resetSafetyUi();
      if (outcome.status === 'already_forgotten') {
        setStatus('That Compare result had already been discarded; the workspace no longer offers it');
      } else if (outcome.cleanup_warning) {
        setStatus(
          `Compare result discarded, but its stored files were not fully removed: ${outcome.cleanup_warning}`,
          'err',
        );
      } else {
        setStatus('Compare result discarded permanently', 'ok');
      }
    } catch (error) {
      setStatus(`The Compare result was not discarded: ${error}`, 'err');
    } finally {
      inFlight.current = null;
      setPending(false);
    }
  }, [dispatch, repository, resetSafetyUi, reviewPending, runInFlight, setStatus]);

  return { forgetResult, forgetPending: pending };
}
