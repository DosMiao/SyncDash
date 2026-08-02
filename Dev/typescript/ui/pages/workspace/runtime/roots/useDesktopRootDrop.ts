import { useEffect, type Dispatch, type MutableRefObject, type SetStateAction } from 'react';
import { getMainWebview } from '#core/infrastructure/tauri/mainWindow.ts';
import { inspectPaths } from '#core/infrastructure/tauri/commands/main.ts';
import type { RootField } from '#core/application/jobs/rootEditor.ts';
import type { StatusApi } from '#ui/shared/status/useStatus.ts';
import type { JobEditorApi } from '#ui/features/jobs/model/job-editor/jobEditorModel.ts';

interface DesktopRootDropOptions {
  dropScope: MutableRefObject<{ editor: HTMLElement | null; path: HTMLElement | null }>;
  editorApi: MutableRefObject<JobEditorApi | null>;
  setDropTargetKey: Dispatch<SetStateAction<string | null>>;
  changeRootDraft: (field: RootField, value: string) => void;
  pushHistory: (path: string) => void;
  setStatus: StatusApi['setMessage'];
}

/// Routes a desktop path drop to the currently mounted root-editing surface. Tauri reports physical
/// coordinates, so hit testing converts them to CSS pixels before consulting the registered scope.
export function useDesktopRootDrop({
  dropScope,
  editorApi,
  setDropTargetKey,
  changeRootDraft,
  pushHistory,
  setStatus,
}: DesktopRootDropOptions) {
  useEffect(() => {
    let disposed = false;
    let dispose: (() => void) | undefined;
    getMainWebview().onDragDropEvent((event) => {
      const payload = event.payload as unknown as {
        type: string;
        paths?: string[];
        position?: { x: number; y: number };
      };
      if (payload.type === 'leave') { setDropTargetKey(null); return; }
      const position = payload.position;
      if (!position) return;
      const pixelRatio = window.devicePixelRatio || 1;
      const x = position.x / pixelRatio;
      const y = position.y / pixelRatio;
      const scope = dropScope.current.editor ?? dropScope.current.path;
      const input = [...(scope?.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('[data-drop]') ?? [])]
        .filter((candidate) => !candidate.disabled)
        .find((candidate) => {
          const bounds = candidate.getBoundingClientRect();
          return x >= bounds.left && x <= bounds.right && y >= bounds.top && y <= bounds.bottom;
        });
      const key = input?.dataset.root ?? input?.dataset.fieldKey ?? null;
      if (payload.type === 'over' || payload.type === 'enter') { setDropTargetKey(key); return; }
      if (payload.type !== 'drop') return;
      setDropTargetKey(null);
      const firstPath = payload.paths?.[0];
      if (!input || !key || !firstPath) return;
      void (async () => {
        // A root field wants a directory; use a dropped file's parent directory.
        let pathValue = firstPath;
        try {
          const pathInformation = await inspectPaths(firstPath, '');
          if (pathInformation.source.readiness === 'not_directory') {
            const separatorIndex = Math.max(pathValue.lastIndexOf('\\'), pathValue.lastIndexOf('/'));
            if (separatorIndex > 0) pathValue = pathValue.slice(0, separatorIndex);
          }
        } catch (error) {
          setStatus(`Could not inspect the dropped path, so it was kept as-is: ${error}`, 'err');
        }
        pushHistory(pathValue);
        if (input.dataset.root === 'source' || input.dataset.root === 'target') {
          changeRootDraft(input.dataset.root as RootField, pathValue);
          setStatus(`Filled the ${input.dataset.root} draft → ${pathValue}. Choose Save to update the job.`);
        } else {
          editorApi.current?.setField(key, pathValue);
          setStatus(`Filled in: ${pathValue}`);
        }
      })();
    })
      // The unlisten handle can arrive after cleanup when StrictMode double-mounts in development.
      .then((unlisten) => { if (disposed) unlisten(); else dispose = unlisten; })
      .catch((error) => {
        if (!disposed) setStatus(`Desktop drag and drop is unavailable: ${error}`, 'err');
      });
    return () => { disposed = true; dispose?.(); };
  }, [changeRootDraft, dropScope, editorApi, pushHistory, setDropTargetKey, setStatus]);
}
