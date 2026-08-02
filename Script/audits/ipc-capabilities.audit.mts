import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function source(path: string): Promise<string> {
  return readFile(join(repositoryRoot, path), 'utf8');
}

async function commandAdapterSource(directory: string): Promise<string> {
  const absoluteDirectory = join(repositoryRoot, directory);
  const entries = await readdir(absoluteDirectory, { withFileTypes: true });
  const sources = await Promise.all(entries.map(async (entry) => {
    const path = join(absoluteDirectory, entry.name);
    if (entry.isDirectory()) {
      return commandAdapterSource(join(directory, entry.name));
    }
    return entry.isFile() && entry.name.endsWith('.ts')
      ? readFile(path, 'utf8')
      : '';
  }));
  return sources.join('\n');
}

async function frontendTreeSource(directory: string): Promise<string> {
  const absoluteDirectory = join(repositoryRoot, directory);
  const entries = await readdir(absoluteDirectory, { withFileTypes: true });
  const sources = await Promise.all(entries.map(async (entry) => {
    const path = join(absoluteDirectory, entry.name);
    if (entry.isDirectory()) return frontendTreeSource(join(directory, entry.name));
    return entry.isFile() && /\.tsx?$/.test(entry.name) ? readFile(path, 'utf8') : '';
  }));
  return sources.join('\n');
}

async function rustTreeSource(directory: string): Promise<string> {
  const absoluteDirectory = join(repositoryRoot, directory);
  const entries = await readdir(absoluteDirectory, { withFileTypes: true });
  const sources = await Promise.all(entries.map(async (entry) => {
    const path = join(absoluteDirectory, entry.name);
    if (entry.isDirectory()) return rustTreeSource(join(directory, entry.name));
    return entry.isFile() && entry.name.endsWith('.rs') ? readFile(path, 'utf8') : '';
  }));
  return sources.join('\n');
}

function sorted(values: Iterable<string>): string[] {
  return [...values].sort();
}

function commandPermissions(toml: string): Set<string> {
  return new Set([...toml.matchAll(/"allow-([a-z0-9-]+)"/g)]
    .map((match) => match[1].replaceAll('-', '_')));
}

const [mainRs, buildRs, commandAdapters, mainPermissionsToml, progressPermissionsToml] = await Promise.all([
  source('Dev/src-tauri/src/app/mod.rs'),
  source('Dev/src-tauri/build.rs'),
  commandAdapterSource('Dev/typescript/core/infrastructure/tauri/commands'),
  source('Dev/src-tauri/permissions/main.toml'),
  source('Dev/src-tauri/permissions/progress.toml'),
]);

const handlerBlock = mainRs.match(/tauri::generate_handler!\[([\s\S]*?)\]\)/)?.[1];
assert.ok(handlerBlock, 'the Tauri app composition must contain one generate_handler command registry');
const registeredHandlers = new Set([...handlerBlock.matchAll(/commands::(?:[a-z_]+::)+([a-z_]+)/g)]
  .map((match) => match[1]));

const manifestBlock = buildRs.match(/const APP_COMMANDS:[\s\S]*?=\s*&\[([\s\S]*?)\];/)?.[1];
assert.ok(manifestBlock, 'build.rs must contain the application command manifest');
const manifestCommands = new Set([...manifestBlock.matchAll(/"([a-z_]+)"/g)]
  .map((match) => match[1]));

const frontendCommands = new Set([...commandAdapters.matchAll(/invoke(?:<[^;\n]+>)?\s*\(\s*['"]([a-z_]+)['"]/g)]
  .map((match) => match[1]));
const mainCommands = commandPermissions(mainPermissionsToml);
const progressCommands = commandPermissions(progressPermissionsToml);

test('handler, application manifest, frontend wrappers, and permissions have exact parity', () => {
  const permissionCommands = new Set([...mainCommands, ...progressCommands]);
  assert.deepEqual(sorted(manifestCommands), sorted(registeredHandlers));
  assert.deepEqual(sorted(frontendCommands), sorted(registeredHandlers));
  assert.deepEqual(sorted(permissionCommands), sorted(registeredHandlers));
  assert.equal(mainCommands.size + progressCommands.size, permissionCommands.size, 'window command sets must not overlap');
});

test('progress authority is limited to lifecycle and Apply controls', () => {
  assert.deepEqual(sorted(progressCommands), sorted([
    'acknowledge_progress_launch',
    'begin_progress_window_close',
    'cancel_apply_run',
    'destroy_progress_window',
    'execute_post_run_power_action',
    'replay_apply_events',
    'report_progress_window_mounted',
    'set_apply_paused',
  ]));
  assert.ok(mainCommands.has('cancel_compare_run'));
  assert.ok(mainCommands.has('replay_compare_events'));
  assert.equal(progressCommands.has('cancel_compare_run'), false);
  assert.equal(progressCommands.has('compare_job'), false);
  assert.equal(progressCommands.has('apply_job'), false);
});

test('window pages expose only their own capability surface', async () => {
  // The workspace page used to reach every main-window command family through one barrel, so this
  // could assert a single import specifier. With the barrel gone it asserts the boundary itself:
  // each page names the families it uses, and neither page names the other window's.
  const [mainWindow, workspacePage, progressWindow, progressPage, progressAdapter] =
    await Promise.all([
      source('Dev/typescript/ui/windows/main/MainWindow.tsx'),
      frontendTreeSource('Dev/typescript/ui/pages/workspace'),
      source('Dev/typescript/ui/windows/progress/ProgressWindow.tsx'),
      frontendTreeSource('Dev/typescript/ui/pages/progress'),
      source('Dev/typescript/core/infrastructure/tauri/commands/progress.ts'),
    ]);
  assert.match(mainWindow, /pages\/workspace\/WorkspacePage\.tsx/);
  assert.match(workspacePage, /commands\/(?:apply|compare|jobs|operations)\.ts/);
  assert.doesNotMatch(workspacePage, /commands\/progress\.ts/);
  assert.match(progressWindow, /pages\/progress\/ProgressPage\.tsx/);
  assert.match(progressPage, /commands\/progress\.ts/);
  assert.doesNotMatch(
    progressPage,
    /commands\/(?:apply|autoscan|compare|jobs|logs|operations|paths|settings)\.ts/,
  );
  assert.doesNotMatch(
    progressAdapter,
    /from\s+['"]\.\/(?:apply|autoscan|compare|jobs|logs|operations|paths|settings)\.ts['"]/,
    'the progress adapter must not re-enter main-window authority',
  );
});

test('Tauri features depend on window identity, never the IPC boundary', async () => {
  const [features, windowIdentity, ipcBoundary] = await Promise.all([
    rustTreeSource('Dev/src-tauri/src/features'),
    source('Dev/src-tauri/src/window.rs'),
    source('Dev/src-tauri/src/ipc/mod.rs'),
  ]);
  assert.doesNotMatch(features, /\bcrate::ipc(?:::|\s*\{)/);
  assert.match(windowIdentity, /MAIN_WINDOW_LABEL/);
  assert.match(windowIdentity, /PROGRESS_WINDOW_LABEL/);
  assert.match(ipcBoundary, /crate::window::\{MAIN_WINDOW_LABEL, PROGRESS_WINDOW_LABEL\}/);
});

test('capabilities are explicit, split by window, and grant no frontend event emission', async () => {
  const [config, mainCapability, progressCapability, progressApp] = await Promise.all([
    source('Dev/src-tauri/tauri.conf.json').then(JSON.parse),
    source('Dev/src-tauri/capabilities/main.json').then(JSON.parse),
    source('Dev/src-tauri/capabilities/progress.json').then(JSON.parse),
    frontendTreeSource('Dev/typescript/ui/pages/progress'),
  ]);
  assert.deepEqual(config.app.security.capabilities, ['main-window', 'progress-window']);
  assert.deepEqual(mainCapability.windows, ['main']);
  assert.deepEqual(progressCapability.windows, ['progress']);
  assert.ok(mainCapability.permissions.includes('main-commands'));
  assert.ok(progressCapability.permissions.includes('progress-commands'));
  const allPermissions = [...mainCapability.permissions, ...progressCapability.permissions];
  assert.equal(allPermissions.includes('core:default'), false);
  assert.equal(allPermissions.some((permission) => /^core:event:allow-emit(?:-to)?$/.test(permission)), false);
  assert.equal(
    progressCapability.permissions.some((permission: string) => permission.startsWith('dialog:')),
    false,
  );
  assert.doesNotMatch(progressApp, /import\s*\{[^}]*\bemit\b[^}]*\}\s*from\s*['"]@tauri-apps\/api\/event['"]/s);
});
