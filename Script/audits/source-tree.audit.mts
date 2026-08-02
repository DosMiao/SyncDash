import assert from 'node:assert/strict';
import { readdir } from 'node:fs/promises';
import { basename, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));
const forbiddenBuckets = new Set([
  'common',
  'helper',
  'helpers',
  'misc',
  'support',
  'util',
  'utils',
  'utility',
  'utilities',
]);

async function directories(root: string): Promise<string[]> {
  const entries = await readdir(root, { withFileTypes: true });
  const children = entries.filter((entry) => entry.isDirectory()).map((entry) => join(root, entry.name));
  const nested = await Promise.all(children.map(directories));
  return [...children, ...nested.flat()];
}

async function files(root: string): Promise<string[]> {
  const entries = await readdir(root, { withFileTypes: true });
  const ownFiles = entries.filter((entry) => entry.isFile()).map((entry) => join(root, entry.name));
  const nested = await Promise.all(entries
    .filter((entry) => entry.isDirectory())
    .map((entry) => files(join(root, entry.name))));
  return [...ownFiles, ...nested.flat()];
}

test('product source has no generic ownership names', async () => {
  const roots = [
    join(repositoryRoot, 'Dev/src'),
    join(repositoryRoot, 'Dev/src-tauri/src'),
    join(repositoryRoot, 'Dev/typescript'),
  ];
  const productDirectories = (await Promise.all(roots.map(directories))).flat();
  const productFiles = (await Promise.all(roots.map(files))).flat();

  for (const path of productDirectories) {
    const name = basename(path).toLowerCase();
    assert.equal(
      forbiddenBuckets.has(name),
      false,
      `${relative(repositoryRoot, path)} is a generic bucket; name the responsibility it owns`,
    );
    assert.notEqual(
      (await readdir(path)).length,
      0,
      `${relative(repositoryRoot, path)} is empty; remove ceremonial source branches`,
    );
  }

  for (const path of productFiles) {
    const stem = basename(path).replace(/\.[^.]+$/, '').toLowerCase();
    assert.equal(
      forbiddenBuckets.has(stem),
      false,
      `${relative(repositoryRoot, path)} has a generic name; name the responsibility it owns`,
    );
  }
});
