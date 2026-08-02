import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));
const typescriptRoot = join(repositoryRoot, 'typescript');
const stylesheetSource = await readFile(join(typescriptRoot, 'styles.css'), 'utf8');

async function sourceFilesUnder(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFilesUnder(path);
    return entry.isFile() && path.endsWith('.tsx') ? [path] : [];
  }));
  return nested.flat();
}

const componentSources = new Map<string, string>();
for (const relativePath of [
  'progress/ProgressApp.tsx',
  'ui/components/AdvancedFiltersPopover.tsx',
  'ui/components/ComparePanel.tsx',
  'ui/components/RunScopePanel.tsx',
  'ui/components/ui.tsx',
]) {
  componentSources.set(relativePath, await readFile(join(typescriptRoot, relativePath), 'utf8'));
}

function occurrenceCount(source: string, value: string): number {
  return source.split(value).length - 1;
}

function cssomBindingCount(
  source: string,
  method: 'setProperty' | 'removeProperty',
  property: string,
): number {
  const escapedProperty = property.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return [...source.matchAll(new RegExp(`style\\.${method}\\(\\s*['"]${escapedProperty}['"]`, 'g'))].length;
}

test('React source emits no CSP-blocked style attributes', async () => {
  for (const path of await sourceFilesUnder(typescriptRoot)) {
    const source = await readFile(path, 'utf8');
    assert.doesNotMatch(source, /\bstyle\s*=/, path);
  }
});

test('dynamic component geometry uses balanced layout-effect CSSOM bindings', () => {
  const propertiesByComponent = new Map<string, string[]>([
    ['progress/ProgressApp.tsx', ['--countdown-progress-width']],
    ['ui/components/AdvancedFiltersPopover.tsx', ['--advanced-filters-left', '--advanced-filters-top']],
    ['ui/components/ComparePanel.tsx', ['--compare-progress-width']],
    ['ui/components/RunScopePanel.tsx', ['--run-scope-depth', '--run-scope-share']],
    ['ui/components/ui.tsx', ['--floating-panel-left', '--floating-panel-top']],
  ]);

  for (const [path, properties] of propertiesByComponent) {
    const source = componentSources.get(path)!;
    assert.ok(source.includes('useLayoutEffect'), `${path} must update geometry before paint`);
    for (const property of properties) {
      const setters = cssomBindingCount(source, 'setProperty', property);
      const removers = cssomBindingCount(source, 'removeProperty', property);
      assert.ok(setters > 0, `${path} must set ${property}`);
      assert.equal(removers, setters, `${path} must clean every ${property} binding`);
      assert.ok(stylesheetSource.includes(`var(${property}`), `${property} must have a static CSS consumer`);
    }
  }
});

test('floating panels start offscreen and maintain balanced viewport listeners', () => {
  assert.ok(stylesheetSource.includes('var(--floating-panel-top, -9999px)'));
  assert.ok(stylesheetSource.includes('var(--floating-panel-left, -9999px)'));
  assert.ok(stylesheetSource.includes('var(--advanced-filters-top, -9999px)'));
  assert.ok(stylesheetSource.includes('var(--advanced-filters-left, -9999px)'));

  for (const path of ['ui/components/ui.tsx', 'ui/components/AdvancedFiltersPopover.tsx']) {
    const source = componentSources.get(path)!;
    const resizeAdds = occurrenceCount(source, "window.addEventListener('resize', updatePosition)");
    const resizeRemoves = occurrenceCount(source, "window.removeEventListener('resize', updatePosition)");
    const scrollAdds = occurrenceCount(source, "document.addEventListener('scroll', updatePosition, true)");
    const scrollRemoves = occurrenceCount(source, "document.removeEventListener('scroll', updatePosition, true)");
    assert.ok(resizeAdds > 0, `${path} must reclamp on viewport resize`);
    assert.equal(resizeRemoves, resizeAdds, `${path} must remove every resize listener`);
    assert.ok(scrollAdds > 0, `${path} must react to ancestor scrolling`);
    assert.equal(scrollRemoves, scrollAdds, `${path} must remove every scroll listener`);
  }
});
