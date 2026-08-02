import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const planTableSource = await readFile(
  new URL('../../typescript/ui/components/PlanTable.tsx', import.meta.url),
  'utf8',
);
const stylesheetSource = await readFile(
  new URL('../../typescript/styles.css', import.meta.url),
  'utf8',
);

test('PlanTable emits no CSP-blocked JSX style attributes', () => {
  assert.doesNotMatch(planTableSource, /\bstyle\s*=/);
});

test('PlanTable CSSOM geometry bindings have stylesheet consumers and cleanup', () => {
  const geometryProperties = [
    '--plan-table-column-width',
    '--tree-depth',
    '--plan-table-minimum-width',
    '--plan-table-canvas-height',
    '--plan-table-body-top',
  ];

  for (const property of geometryProperties) {
    assert.ok(planTableSource.includes(`style.setProperty('${property}'`), `${property} must be set`);
    assert.ok(
      planTableSource.includes(`style.removeProperty('${property}'`),
      `${property} must be removed during its lifecycle`,
    );
    assert.ok(stylesheetSource.includes(`var(${property}`), `${property} must be consumed by CSS`);
  }
});

test('PlanTable exposes one virtualized grid or treegrid with logical row positions', () => {
  assert.match(planTableSource, /role=\{grouped \? 'treegrid' : 'grid'\}/);
  assert.match(planTableSource, /aria-rowcount=\{rowPlan\.length \+ 1\}/);
  assert.match(planTableSource, /aria-rowindex=\{logicalRowIndex \+ 2\}/);
  assert.match(planTableSource, /data-plan-logical-row=\{logicalRowIndex\}/);
  assert.match(planTableSource, /requestedActiveRowIndex >= virtualWindow\.from/);
  assert.match(planTableSource, /rovingTabStopRowIndex/);
  assert.doesNotMatch(planTableSource, /tabIndex=\{0\}/);
  assert.ok((planTableSource.match(/tabIndex=\{isActiveRow \? 0 : -1\}/g) ?? []).length >= 4);
});

test('PlanTable keyboard navigation reaches rows, controls, disclosure, and context menus', () => {
  for (const key of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End']) {
    assert.ok(planTableSource.includes(`event.key === '${key}'`), `${key} must be handled`);
  }
  assert.match(planTableSource, /requestPlanRowFocus\(targetRowIndex\)/);
  assert.match(planTableSource, /renderedRow\.focus\(\{ preventScroll: true \}\)/);
  assert.match(planTableSource, /renderedRow\.scrollIntoView\(\{ block: 'nearest', inline: 'nearest' \}\)/);
  assert.match(planTableSource, /event\.key === 'ContextMenu' \|\| \(event\.shiftKey && event\.key === 'F10'\)/);
  assert.match(planTableSource, /event\.target === event\.currentTarget[\s\S]*event\.key === ' '/);
});

test('PlanTable announces tree disclosure and Synchronize selection without conflating run scope', () => {
  assert.match(planTableSource, /aria-level=\{row\.depth \+ 1\}/);
  assert.match(planTableSource, /aria-expanded=\{!isFolderFolded\}/);
  assert.match(planTableSource, /aria-describedby=\{synchronizationStatusId\}/);
  assert.match(planTableSource, />Sync<\/span>/);
  assert.match(planTableSource, /Select all in-scope executable actions for Synchronize/);
  assert.match(planTableSource, /Selected for Synchronize/);
  assert.doesNotMatch(planTableSource, /Include[^\n]*Run Scope|Exclude[^\n]*Run Scope/);
  assert.match(stylesheetSource, /\.plan-table \.c-synchronize \{ width: 58px; text-align: center; \}/);
  assert.match(stylesheetSource, /\.plan-table tbody tr\[tabindex\]:focus-within/);
});
