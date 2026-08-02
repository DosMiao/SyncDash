import { useEffect, type Dispatch } from 'react';
import { listenToMainWindowEvent } from '#core/infrastructure/tauri/mainWindow.ts';
import type { CompareScopeExecutionStatusDto } from '#core/types/generated/CompareScopeExecutionStatusDto.ts';
import type { CompareWorkspaceAction } from '#core/application/compare-workspace/compareWorkspaceRepository.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';

export function useCompareExecutionStatusSubscription(options: {
  dispatch: Dispatch<CompareWorkspaceAction>;
  setStatus: StatusApi['setMessage'];
}) {
  const { dispatch, setStatus } = options;
  useEffect(() => {
    let disposed = false;
    let remove: (() => void) | null = null;
    void listenToMainWindowEvent<CompareScopeExecutionStatusDto>(
      'compare-execution-status',
      ({ payload }) => {
        if (!disposed) dispatch({ type: 'execution_status_received', execution: payload });
      },
    ).then((unlisten) => {
      if (disposed) unlisten(); else remove = unlisten;
    }).catch((error) => {
      if (!disposed) setStatus(`Compare execution-status subscription failed: ${error}`, 'err');
    });
    return () => {
      disposed = true;
      remove?.();
    };
  }, [dispatch, setStatus]);
}
