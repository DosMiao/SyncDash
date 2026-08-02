import { useLayoutEffect, useState } from 'react';

/// Observe the container because adjacent panels can change its width without resizing the window.
export function useContainerWidth(container: HTMLElement | null): number {
  const [containerWidthPixels, setContainerWidthPixels] = useState(1600);
  useLayoutEffect(() => {
    if (!container) return;
    const observer = new ResizeObserver(() => setContainerWidthPixels(container.clientWidth));
    observer.observe(container);
    setContainerWidthPixels(container.clientWidth);
    return () => observer.disconnect();
  }, [container]);
  return containerWidthPixels;
}
