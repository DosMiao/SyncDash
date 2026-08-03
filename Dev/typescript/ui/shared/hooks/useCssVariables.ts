import { useLayoutEffect, useRef, type RefObject } from 'react';

/**
 * Write CSS custom properties onto a ref'd element and take them back off on cleanup.
 *
 * The Tauri CSP blocks `style=""` attributes, so every geometry value React computes has to reach
 * the stylesheet through the CSSOM instead. A `null` value removes the property rather than
 * writing an empty string, which is how a caller expresses "this element has no value for it".
 *
 * `deps` is explicit because `variables` is a fresh object literal on every render: virtualized
 * table rows would otherwise rewrite and re-remove their properties on each of their own renders.
 */
export function useCssVariables<E extends HTMLElement>(
  target: RefObject<E | null>,
  variables: Record<string, string | null>,
  deps: readonly unknown[],
): void {
  const latestVariables = useRef(variables);
  latestVariables.current = variables;

  useLayoutEffect(() => {
    const element = target.current;
    if (!element) return;
    const names = Object.keys(latestVariables.current);
    for (const name of names) {
      const value = latestVariables.current[name];
      if (value === null) element.style.removeProperty(name);
      else element.style.setProperty(name, value);
    }
    return () => {
      for (const name of names) element.style.removeProperty(name);
    };
  }, [target, ...deps]);
}
