import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function source(path: string): Promise<string> {
  return readFile(join(repositoryRoot, path), 'utf8');
}

function sorted(values: Iterable<string>): string[] {
  return [...values].sort();
}

function commandPermissions(toml: string): Set<string> {
  return new Set([...toml.matchAll(/"allow-([a-z0-9-]+)"/g)]
    .map((match) => match[1].replaceAll('-', '_')));
}

const [mainRs, buildRs, ipcTs, mainPermissionsToml, progressPermissionsToml] = await Promise.all([
  source('src-tauri/src/main.rs'),
  source('src-tauri/build.rs'),
  source('typescript/core/ipc.ts'),
  source('src-tauri/permissions/main.toml'),
  source('src-tauri/permissions/progress.toml'),
]);

const handlerBlock = mainRs.match(/tauri::generate_handler!\[([\s\S]*?)\]\)/)?.[1];
assert.ok(handlerBlock, 'main.rs must contain one generate_handler command registry');
const registeredHandlers = new Set([...handlerBlock.matchAll(/cmd::[a-z_]+::([a-z_]+)/g)]
  .map((match) => match[1]));

const manifestBlock = buildRs.match(/const APP_COMMANDS:[\s\S]*?=\s*&\[([\s\S]*?)\];/)?.[1];
assert.ok(manifestBlock, 'build.rs must contain the application command manifest');
const manifestCommands = new Set([...manifestBlock.matchAll(/"([a-z_]+)"/g)]
  .map((match) => match[1]));

const frontendCommands = new Set([...ipcTs.matchAll(/invoke(?:<[^;\n]+>)?\s*\(\s*['"]([a-z_]+)['"]/g)]
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

test('capabilities are explicit, split by window, and grant no frontend event emission', async () => {
  const [config, mainCapability, progressCapability, progressApp] = await Promise.all([
    source('src-tauri/tauri.conf.json').then(JSON.parse),
    source('src-tauri/capabilities/main.json').then(JSON.parse),
    source('src-tauri/capabilities/progress.json').then(JSON.parse),
    source('typescript/progress/ProgressApp.tsx'),
  ]);
  assert.deepEqual(config.app.security.capabilities, ['main-window', 'progress-window']);
  assert.deepEqual(mainCapability.windows, ['main']);
  assert.deepEqual(progressCapability.windows, ['progress']);
  assert.ok(mainCapability.permissions.includes('main-commands'));
  assert.ok(progressCapability.permissions.includes('progress-commands'));
  const allPermissions = [...mainCapability.permissions, ...progressCapability.permissions];
  assert.equal(allPermissions.includes('core:default'), false);
  assert.equal(allPermissions.some((permission) => /^core:event:allow-emit(?:-to)?$/.test(permission)), false);
  assert.equal(progressCapability.permissions.some((permission) => permission.startsWith('dialog:')), false);
  assert.doesNotMatch(progressApp, /import\s*\{[^}]*\bemit\b[^}]*\}\s*from\s*['"]@tauri-apps\/api\/event['"]/s);
});
