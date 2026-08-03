import { useLayoutEffect, useRef } from 'react';

interface OwnedResultViewport {
  scrollTop: number;
  scrollLeft: number;
}

export function useOwnedResultViewport<WorkspaceKey extends string>(
  element: HTMLElement | null,
  workspaceKey: WorkspaceKey | null,
  viewport: OwnedResultViewport,
  onViewportChange: (workspaceKey: WorkspaceKey, viewport: OwnedResultViewport) => void,
): void {
  const activeWorkspaceKey = useRef(workspaceKey);
  useLayoutEffect(() => {
    activeWorkspaceKey.current = workspaceKey;
  }, [workspaceKey]);

  useLayoutEffect(() => {
    if (!element || workspaceKey === null) return;
    element.scrollTop = viewport.scrollTop;
    element.scrollLeft = viewport.scrollLeft;
  }, [element, viewport.scrollLeft, viewport.scrollTop, workspaceKey]);

  useLayoutEffect(() => {
    if (!element || workspaceKey === null) return;
    let animationFrame: number | null = null;
    let pendingViewport: OwnedResultViewport | null = null;
    const capture = () => {
      if (activeWorkspaceKey.current !== workspaceKey) return;
      pendingViewport = {
        scrollTop: element.scrollTop,
        scrollLeft: element.scrollLeft,
      };
      if (animationFrame !== null) return;
      animationFrame = requestAnimationFrame(() => {
        animationFrame = null;
        if (activeWorkspaceKey.current !== workspaceKey || pendingViewport === null) return;
        onViewportChange(workspaceKey, pendingViewport);
        pendingViewport = null;
      });
    };
    element.addEventListener('scroll', capture, { passive: true });
    return () => {
      element.removeEventListener('scroll', capture);
      if (animationFrame !== null) cancelAnimationFrame(animationFrame);
      if (pendingViewport !== null) onViewportChange(workspaceKey, pendingViewport);
    };
  }, [element, onViewportChange, workspaceKey]);
}
