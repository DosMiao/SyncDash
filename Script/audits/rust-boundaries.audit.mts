import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function rustFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return rustFiles(path);
    return entry.isFile() && entry.name.endsWith('.rs') ? [path] : [];
  }));
  return nested.flat();
}

async function assertFilesAvoid(
  directory: string,
  forbidden: RegExp,
  boundary: string,
): Promise<void> {
  for (const path of await rustFiles(join(repositoryRoot, directory))) {
    const contents = await readFile(path, 'utf8');
    assert.doesNotMatch(
      contents,
      forbidden,
      `${relative(repositoryRoot, path)} crosses the ${boundary} boundary`,
    );
  }
}

function crateModules(...names: string[]): RegExp {
  const alternatives = names.join('|');
  return new RegExp(
    `\\bcrate::(?:${alternatives})\\b|\\bcrate::\\{[^}]*\\b(?:${alternatives})\\b`,
  );
}

test('core Rust dependencies point inward through the physical tree', async () => {
  await Promise.all([
    assertFilesAvoid(
      'Dev/src/base',
      crateModules('obs', 'store', 'pipeline', 'transfer', 'boot', 'job', 'run'),
      'base -> higher layer',
    ),
    assertFilesAvoid(
      'Dev/src/services',
      crateModules('pipeline', 'transfer', 'boot', 'job', 'run'),
      'services -> workflow/application',
    ),
    assertFilesAvoid(
      'Dev/src/workflow',
      crateModules('boot', 'job', 'run', 'cli'),
      'workflow -> application/shell',
    ),
    assertFilesAvoid(
      'Dev/src/application',
      crateModules('cli'),
      'application -> shell',
    ),
  ]);
});

test('Tauri contracts and shared primitives remain dependency leaves', async () => {
  await assertFilesAvoid(
    'Dev/src-tauri/src/contracts',
    crateModules('app', 'features', 'ipc'),
    'contracts -> stateful/composition layer',
  );

  for (const repositoryPath of [
    'Dev/src-tauri/src/secure_random.rs',
    'Dev/src-tauri/src/window.rs',
  ]) {
    const contents = await readFile(join(repositoryRoot, repositoryPath), 'utf8');
    assert.doesNotMatch(
      contents,
      crateModules('app', 'features', 'ipc'),
      `${repositoryPath} must remain a dependency leaf`,
    );
  }
});

test('Tauri IPC never depends on the application composition root', async () => {
  await Promise.all([
    assertFilesAvoid(
      'Dev/src-tauri/src/features',
      crateModules('app', 'ipc'),
      'feature -> delivery/composition layer',
    ),
    assertFilesAvoid(
      'Dev/src-tauri/src/ipc',
      crateModules('app'),
      'IPC -> app composition',
    ),
  ]);
});

test('Tauri operation delivery delegates to feature-owned use cases', async () => {
  await Promise.all([
    assertFilesAvoid(
      'Dev/src-tauri/src/ipc/commands/operations',
      /\bsyncdash::|\bspawn_blocking\b/,
      'operation delivery -> engine execution',
    ),
    assertFilesAvoid(
      'Dev/src-tauri/src/features',
      /#\[tauri::command\]/,
      'feature -> command delivery annotation',
    ),
  ]);
});
