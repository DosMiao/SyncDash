import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import ts from 'typescript';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function source(path: string): Promise<string> {
  return readFile(join(repositoryRoot, path), 'utf8');
}

async function tsxFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return tsxFiles(path);
    return entry.isFile() && entry.name.endsWith('.tsx') ? [path] : [];
  }));
  return nested.flat();
}

function functionBlock(contents: string, start: string, end: string): string {
  const startIndex = contents.indexOf(start);
  const endIndex = contents.indexOf(end, startIndex + start.length);
  assert.ok(startIndex >= 0, `missing block start: ${start}`);
  assert.ok(endIndex > startIndex, `missing block end: ${end}`);
  return contents.slice(startIndex, endIndex);
}

test('every React button in the main and progress surfaces declares its native type', async () => {
  const roots = [
    join(repositoryRoot, 'typescript/ui'),
    join(repositoryRoot, 'typescript/progress'),
  ];
  const files = (await Promise.all(roots.map(tsxFiles))).flat();
  let buttonCount = 0;

  for (const path of files) {
    const contents = await readFile(path, 'utf8');
    const parsed = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const inspect = (node: ts.Node) => {
      if ((ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node))
        && node.tagName.getText(parsed) === 'button'
      ) {
        buttonCount += 1;
        const typeAttribute = node.attributes.properties.find((attribute) => (
          ts.isJsxAttribute(attribute) && attribute.name.getText(parsed) === 'type'
        ));
        assert.ok(typeAttribute, `${path}:${parsed.getLineAndCharacterOfPosition(node.pos).line + 1} lacks button type`);
      }
      ts.forEachChild(node, inspect);
    };
    inspect(parsed);
  }

  assert.ok(buttonCount >= 30);
});

test('destructive dialogs and advanced-filter dismissal preserve a safe focus target', async () => {
  const [ui, advancedFilters, runScope] = await Promise.all([
    source('typescript/ui/components/ui.tsx'),
    source('typescript/ui/components/AdvancedFiltersPopover.tsx'),
    source('typescript/ui/components/RunScopePanel.tsx'),
  ]);

  assert.match(ui, /const dangerConfirmation = actions\.some\(\(action\) => action\.danger && !action\.disabled\)/);
  assert.match(ui, /autoFocus=\{dangerConfirmation\} onClick=\{onCancel\}>Cancel/);
  assert.match(ui, /autoFocus=\{!dangerConfirmation && index === firstEnabledAction\}/);

  assert.match(advancedFilters, /useState\(\(\) => createAdvancedFilterDraft\(appliedFilter\)\)/);
  assert.match(advancedFilters, /handlers: \{ dismiss: \(\) => dismiss\(true\) \}/);
  assert.match(advancedFilters, /if \(restoreFocus\) restorePreviousFocus\(\)/);

  const treeItem = functionBlock(runScope, 'role="treeitem"', '</div>');
  assert.match(treeItem, /aria-expanded=/);
  assert.match(runScope, /event\.key === 'Enter' \|\| event\.key === ' '/);
  const expander = functionBlock(runScope, 'className="run-scope-folder-expand"', '</span>');
  assert.match(expander, /onToggleExpandedFolder\(node\.path\)/);
  assert.match(runScope, /<span className="run-scope-folder-chevron" aria-hidden="true" \/>/);
});

test('CSV export is one owned request with captured result identity and truthful pending UI', async () => {
  const [app, resultBar] = await Promise.all([
    source('typescript/ui/App.tsx'),
    source('typescript/ui/components/ResultBar.tsx'),
  ]);
  const exportBlock = functionBlock(app, 'const exportCsv = useCallback', 'const changeRootDraft');
  const guard = exportBlock.indexOf('if (csvExportInFlight.current !== null) return');
  const claim = exportBlock.indexOf('csvExportInFlight.current = resultKey');
  const request = exportBlock.indexOf('await ipc.exportCompareCsv');

  assert.ok(guard >= 0 && guard < claim && claim < request);
  assert.ok(exportBlock.indexOf('const compareIdentity = selectedCompareWorkspace.identity') < request);
  assert.ok(exportBlock.indexOf('const rowPresentation = layout.displayOrder.map') < request);
  assert.match(exportBlock, /selectedCompareWorkspaceKeyRef\.current === resultKey/);
  assert.match(exportBlock, /if \(csvExportInFlight\.current === resultKey\)[\s\S]*setCsvExportPending\(false\)/);
  assert.match(resultBar, /disabled=\{exportPending \|\| scopeCalculationPending/);
  assert.match(resultBar, /\{exportPending \? 'Exporting…' : 'Export CSV'\}/);
  assert.match(app, /`Undo \$\{field\} change`/);
});

test('status actions are imperatively single-flight and failed undo remains retryable', async () => {
  const [statusHook, statusBar, app] = await Promise.all([
    source('typescript/ui/hooks/useStatus.ts'),
    source('typescript/ui/components/StatusBar.tsx'),
    source('typescript/ui/App.tsx'),
  ]);

  assert.match(statusHook, /new StatusAuthority\(initialMessage\)/);
  assert.match(statusHook, /authority\.executeAction\(actionId\)/);
  assert.match(statusBar, /disabled=\{status\.actionPending\}/);
  assert.match(statusBar, /status\.notices\.map/);
  assert.match(statusBar, /onAction\(noticeAction\.id\)/);
  assert.equal((app.match(/throw new Error\(await describeMutationFailure\(/g) ?? []).length, 3);
});

test('progress controls fence requests by exact run and preferences publish only after storage succeeds', async () => {
  const progress = await source('typescript/progress/ProgressApp.tsx');
  const pauseBlock = functionBlock(progress, 'const togglePause = useCallback', 'const stopRun = useCallback');
  const stopBlock = functionBlock(progress, 'const stopRun = useCallback', 'const completionPercentage = completionPercent');
  const powerBlock = functionBlock(
    progress,
    'const executePowerAction = useCallback',
    'const reconcilePowerActionCountdown',
  );
  const errorBlock = functionBlock(progress, 'const reportControlError = useCallback', 'const reportWindowChromeFailure');

  assert.ok(pauseBlock.indexOf('pauseRequestRef.current = request') < pauseBlock.indexOf('await setApplyPaused'));
  assert.match(pauseBlock, /pauseRequestRef\.current === request && runStateRef\.current\.runId === request\.runId/);
  assert.ok(stopBlock.indexOf('stopRequestRef.current = request') < stopBlock.indexOf('await cancelApplyRun'));
  assert.match(stopBlock, /stopRequestRef\.current !== request \|\| runStateRef\.current\.runId !== request\.runId/);
  assert.ok(powerBlock.indexOf('powerActionRequestRef.current = request') < powerBlock.indexOf('await executePostRunPowerAction'));
  assert.match(powerBlock, /currentRunState\.runId !== runId/);
  assert.match(powerBlock, /powerActionReadyRunIdRef\.current !== runId/);
  assert.match(powerBlock, /whenFinishedActionRef\.current !== action/);
  assert.doesNotMatch(errorBlock, /setStopState\(/);
  assert.match(progress, /if \(closeRequestPendingRef\.current \|\| windowDestructionPendingRef\.current\) return/);
  assert.match(progress, /readyRunId: powerActionReadyRunIdRef\.current/);

  const autoCloseSave = progress.indexOf('const error = saveAutoClosePreference(localStorage, nextAutoCloseEnabled)');
  const autoClosePublish = progress.indexOf('setAutoCloseEnabled(nextAutoCloseEnabled)', autoCloseSave);
  const whenFinishedSave = progress.indexOf('const error = saveWhenFinishedPreference(localStorage, nextWhenFinishedAction)');
  const whenFinishedPublish = progress.indexOf('setWhenFinishedAction(nextWhenFinishedAction)', whenFinishedSave);
  assert.ok(autoCloseSave >= 0 && autoCloseSave < autoClosePublish);
  assert.ok(whenFinishedSave >= 0 && whenFinishedSave < whenFinishedPublish);
  assert.match(progress, /disabled=\{autoCloseEnabled \|\| powerActionPending !== null\}/);
});

test('progress timers, alerts, and scroll frames have explicit owners and cleanup', async () => {
  const [progress, graph, scrollSpy] = await Promise.all([
    source('typescript/progress/ProgressApp.tsx'),
    source('typescript/progress/Graph.tsx'),
    source('typescript/ui/hooks/useScrollSpy.ts'),
  ]);
  const autoCloseEffect = functionBlock(
    progress,
    'useEffect(() => {\n    if (!scheduledAutoClose) return;',
    'const handleRunProgressEvent',
  );

  assert.match(autoCloseEffect, /const autoCloseTimerId = window\.setTimeout/);
  assert.match(autoCloseEffect, /return \(\) => window\.clearTimeout\(autoCloseTimerId\)/);
  assert.match(autoCloseEffect, /completedRunId: scheduledAutoClose\.runId/);
  assert.match(autoCloseEffect, /currentRunId: currentRunState\.runId/);
  assert.match(progress, /setScheduledAutoClose\(null\);[\s\S]*beginProgressWindowClose/);
  const autoClosePreferenceHandler = functionBlock(
    progress,
    'const nextAutoCloseEnabled = event.target.checked;',
    '/> Auto-close when finished',
  );
  const whenFinishedPreferenceHandler = functionBlock(
    progress,
    'const nextWhenFinishedAction = event.target.value;',
    '<option value="none">',
  );
  assert.match(autoClosePreferenceHandler, /setScheduledAutoClose\(null\)/);
  assert.match(whenFinishedPreferenceHandler, /setScheduledAutoClose\(null\)/);

  assert.match(progress, /role="alertdialog"/);
  assert.match(progress, /aria-live="assertive" aria-atomic="true"/);
  assert.match(progress, /<span aria-hidden="true">[\s\S]*powerActionCountdown\.secondsRemaining/);
  assert.match(progress, /ref=\{countdownCancelButtonRef\}[\s\S]*autoFocus/);
  assert.match(progress, /event\.key !== 'Escape'/);
  assert.match(progress, /role="progressbar"[\s\S]*aria-valuenow=/);
  assert.match(progress, /className="pphase" role="status" aria-live="polite"/);
  assert.match(graph, /<canvas ref=\{canvasRef\} aria-hidden="true"/);

  assert.match(scrollSpy, /pendingAnimationFrameId = requestAnimationFrame/);
  assert.match(scrollSpy, /cancelAnimationFrame\(pendingAnimationFrameId\)/);
});

test('fatal rendering diagnostics do not claim persistence after storage failure', async () => {
  const boundary = await source('typescript/ui/components/AppErrorBoundary.tsx');
  assert.match(boundary, /diagnosticStorage: 'pending' \| 'saved' \| \{ error: string \}/);
  assert.match(boundary, /const storageError = remember\('render error'/);
  assert.match(boundary, /diagnosticStorage: storageError \? \{ error: storageError \} : 'saved'/);
  assert.match(boundary, /Saving a local diagnostic/);
  assert.match(boundary, /The diagnostic could not be saved locally/);
  assert.match(boundary, /The error was saved as <code>sd\.last-ui-error<\/code>/);
});

test('the main window has one keyboard authority and no component-level global shortcut listeners', async () => {
  const uiDirectory = join(repositoryRoot, 'typescript/ui');
  const files = await tsxFiles(uiDirectory);
  const globalKeyboardRegistrations: string[] = [];
  for (const path of files) {
    const contents = await readFile(path, 'utf8');
    if (/addEventListener\(['"]keydown/.test(contents)) globalKeyboardRegistrations.push(path);
  }
  assert.deepEqual(globalKeyboardRegistrations.map((path) => path.replace(`${uiDirectory}/`, '')), [
    'hooks/useInteractionLayer.tsx',
  ]);

  const [provider, ui, app] = await Promise.all([
    source('typescript/ui/hooks/useInteractionLayer.tsx'),
    source('typescript/ui/components/ui.tsx'),
    source('typescript/ui/App.tsx'),
  ]);
  assert.match(provider, /setElementInactive\(applicationRoot, topModalId !== null\)/);
  assert.match(ui, /kind: 'modal'/);
  assert.doesNotMatch(ui, /escapeLayerStack|sheetStack/);
  assert.doesNotMatch(app, /applicationShortcutsEnabled/);
});
