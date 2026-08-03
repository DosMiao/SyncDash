import assert from 'node:assert/strict';
import { readdir } from 'node:fs/promises';
import { basename, join, relative } from 'node:path';
import test from 'node:test';

import { directoriesUnder, filesUnder, repositoryRoot } from './tree.mts';

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

test('product source has no generic ownership names', async () => {
  const roots = [
    join(repositoryRoot, 'Dev/src'),
    join(repositoryRoot, 'Dev/src-tauri/src'),
    join(repositoryRoot, 'Dev/typescript'),
  ];
  const productDirectories = (await Promise.all(roots.map(directoriesUnder))).flat();
  const productFiles = (await Promise.all(
    roots.map((root) => filesUnder(root, () => true)),
  )).flat();

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
