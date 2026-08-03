import assert from 'node:assert/strict';
import test from 'node:test';

import { buildLayout, flattenLayout, layoutFolderPaths } from '#core/domain/compare/grouping.ts';
import type { FolderNode, PlanLayout, RowSpec } from '#core/domain/compare/grouping.ts';
import type { PlanDto, PlanOperation, Sort } from '#core/domain/compare/plan.ts';

function operation(path: string, patch: Partial<PlanOperation> = {}): PlanOperation {
  return {
    side: 'target',
    action: 'copy',
    path,
    size: 1,
    mtime_ms: 1_700_000_000_000,
    reason: 'fixture',
    ...patch,
  };
}

function plan(ops: PlanOperation[]): PlanDto {
  return {
    owner: {} as PlanDto['owner'],
    // Grouping never reads the header. Keep the fixture focused on the paths and row metadata that
    // are its actual input instead of copying the whole Rust wire header into every frontend test.
    header: {} as PlanDto['header'],
    ops,
    metas: ops.map(() => null),
    identical_count: 0,
    identical_bytes: 0,
    mtime_window_ms: 2000,
  };
}

function layoutOf(result: PlanDto, reversedRows: boolean[] = [], sort: Sort | null = null): PlanLayout {
  return buildLayout({
    plan: result,
    reversedRows,
    inScopeIndices: result.ops.map((_, index) => index),
    grouped: true,
    sort,
  });
}

function allFolders(tree: FolderNode[] | null): FolderNode[] {
  if (!tree) return [];
  const out: FolderNode[] = [];
  const visit = (node: FolderNode) => {
    out.push(node);
    for (const child of node.children) visit(child);
  };
  for (const node of tree) visit(node);
  return out;
}

function folder(layout: PlanLayout, folderPath: string): FolderNode {
  const hits = allFolders(layout.folderTree).filter((node) => node.path === folderPath);
  assert.equal(hits.length, 1, `expected exactly one folder node for ${JSON.stringify(folderPath)}`);
  return hits[0];
}

function members(layout: PlanLayout, folderPath: string): number[] {
  const node = folder(layout, folderPath);
  return layout.displayOrder.slice(node.start, node.end);
}

function rowTags(result: PlanDto, rows: RowSpec[]): string[] {
  return rows.map((row) => (
    typeof row === 'number'
      ? `row:${result.ops[row].path}`
      : `folder:${row.folderPath || 'Root Level'}@${row.depth}`
  ));
}

function assertPermutation(actual: number[], expected: number[]): void {
  assert.equal(new Set(actual).size, actual.length, 'the DFS order must not duplicate operations');
  assert.deepEqual(
    [...actual].sort((left, right) => left - right),
    [...expected].sort((left, right) => left - right),
  );
}

test('recursive layout synthesizes every intermediate folder while root files stay a sibling group', () => {
  const result = plan([
    operation('root.txt'),
    operation('docs/readme.md'),
    operation('docs/api/v1/openapi.json'),
    operation('docs/api/v2/schema.json'),
    operation('src/internal/cache.bin'),
  ]);
  const layout = layoutOf(result);

  assert.deepEqual(layout.folderTree?.map((node) => node.path), ['', 'docs', 'src']);
  assert.deepEqual(layoutFolderPaths(layout), [
    '',
    'docs',
    'docs/api',
    'docs/api/v1',
    'docs/api/v2',
    'src',
    'src/internal',
  ]);
  assert.deepEqual(
    allFolders(layout.folderTree).map((node) => [node.path, node.depth]),
    [
      ['', 0],
      ['docs', 0],
      ['docs/api', 1],
      ['docs/api/v1', 2],
      ['docs/api/v2', 2],
      ['src', 0],
      ['src/internal', 1],
    ],
  );

  const root = folder(layout, '');
  assert.deepEqual(root.directIndices, [0], 'the Root Level pseudo-folder owns root files only');
  assert.deepEqual(root.children, [], 'top-level real folders are siblings, not children of Root Level');
  assert.deepEqual(members(layout, ''), [0]);
  assert.deepEqual(folder(layout, 'docs').directIndices, [1]);
  assert.deepEqual(folder(layout, 'docs/api').directIndices, []);
  assert.deepEqual(folder(layout, 'docs/api/v1').directIndices, [2]);
  assert.deepEqual(folder(layout, 'docs/api/v2').directIndices, [3]);
  assert.deepEqual(folder(layout, 'src/internal').directIndices, [4]);
  assertPermutation(layout.displayOrder, [0, 1, 2, 3, 4]);

  assert.deepEqual(rowTags(result, flattenLayout(layout, new Set())), [
    'folder:Root Level@0',
    'row:root.txt',
    'folder:docs@0',
    'row:docs/readme.md',
    'folder:docs/api@1',
    'folder:docs/api/v1@2',
    'row:docs/api/v1/openapi.json',
    'folder:docs/api/v2@2',
    'row:docs/api/v2/schema.json',
    'folder:src@0',
    'folder:src/internal@1',
    'row:src/internal/cache.bin',
  ]);
});

test('collapse hides exactly one folder subtree at each depth and leaves siblings alone', () => {
  const result = plan([
    operation('root.txt'),
    operation('a/own.txt'),
    operation('a/b/own.txt'),
    operation('a/b/c/leaf.txt'),
    operation('z/peer.txt'),
  ]);
  const layout = layoutOf(result);

  assert.deepEqual(rowTags(result, flattenLayout(layout, new Set(['a/b/c']))), [
    'folder:Root Level@0', 'row:root.txt',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1', 'row:a/b/own.txt',
    'folder:a/b/c@2',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(result, flattenLayout(layout, new Set(['a/b']))), [
    'folder:Root Level@0', 'row:root.txt',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(result, flattenLayout(layout, new Set(['a']))), [
    'folder:Root Level@0', 'row:root.txt',
    'folder:a@0',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(result, flattenLayout(layout, new Set(['']))), [
    'folder:Root Level@0',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1', 'row:a/b/own.txt',
    'folder:a/b/c@2', 'row:a/b/c/leaf.txt',
    'folder:z@0', 'row:z/peer.txt',
  ], 'folding Root Level must not fold its top-level folder siblings');
});

test('folder intervals aggregate descendants without copying them into every ancestor', () => {
  const result = plan([
    operation('a/own.bin', { size: 2 }),
    operation('a/b/child.bin', { action: 'update', size: 3 }),
    operation('a/b/report.txt', { action: 'conflict', size: 5 }),
    operation('a/b/c/leaf.bin', { action: 'delete', size: 7 }),
    operation('sibling/note.txt', { action: 'note', size: 11 }),
  ]);
  const layout = layoutOf(result);
  const folderA = folder(layout, 'a');
  const folderAB = folder(layout, 'a/b');
  const folderABC = folder(layout, 'a/b/c');
  const sibling = folder(layout, 'sibling');

  assert.deepEqual(folderA.directIndices, [0]);
  assert.deepEqual(folderAB.directIndices, [1, 2]);
  assert.deepEqual(folderABC.directIndices, [3]);
  assert.deepEqual(members(layout, 'a'), [0, 1, 2, 3]);
  assert.deepEqual(members(layout, 'a/b'), [1, 2, 3]);
  assert.deepEqual(members(layout, 'a/b/c'), [3]);
  assert.deepEqual(members(layout, 'sibling'), [4]);

  assert.deepEqual(
    [
      [folderA.count, folderA.executableCount, folderA.bytes],
      [folderAB.count, folderAB.executableCount, folderAB.bytes],
      [folderABC.count, folderABC.executableCount, folderABC.bytes],
      [sibling.count, sibling.executableCount, sibling.bytes],
    ],
    [
      [4, 3, 17],
      [3, 2, 15],
      [1, 1, 7],
      [1, 0, 11],
    ],
  );
  for (const node of [folderA, folderAB, folderABC, sibling]) {
    assert.equal(node.count, node.end - node.start);
  }
  assert.ok(
    folderA.start <= folderAB.start && folderAB.end <= folderA.end,
    'child intervals must nest inside their parent',
  );
  assert.ok(
    folderAB.start <= folderABC.start && folderABC.end <= folderAB.end,
    'deep child intervals must stay nested',
  );
  assert.ok(folderA.end <= sibling.start, 'sibling intervals must not overlap');
  assert.deepEqual(
    layout.displayOrder
      .slice(folderA.start, folderA.end)
      .filter((index) => !['conflict', 'note'].includes(result.ops[index].action)),
    [0, 1, 3],
    'a parent checkbox maps its interval to executable descendants only',
  );
  assertPermutation(layout.displayOrder, [0, 1, 2, 3, 4]);

  const renderedA = flattenLayout(layout, new Set()).find(
    (row): row is Exclude<RowSpec, number> => typeof row !== 'number' && row.folderPath === 'a',
  );
  assert.ok(renderedA);
  assert.deepEqual(
    {
      start: renderedA.start,
      end: renderedA.end,
      count: renderedA.count,
      executableCount: renderedA.executableCount,
      bytes: renderedA.bytes,
    },
    { start: folderA.start, end: folderA.end, count: 4, executableCount: 3, bytes: 17 },
  );
});

test('action sorting can reorder reversedRows subtrees without changing folder membership', () => {
  const result = plan([
    operation('alpha/deep/copy.bin', { action: 'copy', size: 10 }),
    operation('beta/deep/delete.bin', { action: 'delete', size: 20 }),
  ]);
  const actionAsc: Sort = { key: 'action', dir: 1 };

  const before = layoutOf(result, [false, false], actionAsc);
  assert.deepEqual(layoutFolderPaths(before), ['alpha', 'alpha/deep', 'beta', 'beta/deep']);
  assert.deepEqual(before.displayOrder, [0, 1]);
  assert.deepEqual(members(before, 'alpha'), [0]);
  assert.deepEqual(members(before, 'beta'), [1]);

  // copy -> delete and delete -> copy. The aggregate action rank swaps the two top-level folders,
  // but a display sort must never turn the sorted value into the row's directory identity.
  const after = layoutOf(result, [true, true], actionAsc);
  assert.deepEqual(layoutFolderPaths(after), ['beta', 'beta/deep', 'alpha', 'alpha/deep']);
  assert.deepEqual(after.displayOrder, [1, 0]);
  assert.deepEqual(members(after, 'alpha'), [0]);
  assert.deepEqual(members(after, 'beta'), [1]);
  assertPermutation(after.displayOrder, [0, 1]);
});

test('a cross-directory move is grouped by its destination path, not by a side-specific sort path', () => {
  const result = plan([
    operation('new/deep/moved.txt', {
      side: 'source',
      action: 'move',
      from: 'old/place/moved.txt',
      size: 4,
    }),
    operation('middle/other.txt', { size: 2 }),
  ]);
  const layout = layoutOf(result, [false, false], { key: 's.path', dir: 1 });
  const folderPaths = layoutFolderPaths(layout);

  assert.ok(folderPaths.includes('new'));
  assert.ok(folderPaths.includes('new/deep'));
  assert.ok(!folderPaths.includes('old'));
  assert.ok(!folderPaths.includes('old/place'));
  assert.deepEqual(folder(layout, 'new/deep').directIndices, [0]);
  assert.deepEqual(members(layout, 'new'), [0]);
  assertPermutation(layout.displayOrder, [0, 1]);
});

test('delete_dir is a distinct self row inside the folder its checkbox controls', () => {
  const result = plan([
    operation('a/b', { action: 'delete_dir', size: null }),
    operation('a/b/child.txt', { action: 'delete', size: 3 }),
    operation('a', { action: 'delete_dir', size: null }),
  ]);
  const layout = layoutOf(result);
  const folderPaths = layoutFolderPaths(layout);

  assert.equal(folderPaths.filter((folderPath) => folderPath === 'a').length, 1);
  assert.equal(folderPaths.filter((folderPath) => folderPath === 'a/b').length, 1);
  assert.ok(!folderPaths.includes(''), 'a top-level directory operation is not a root-file row');
  assert.deepEqual(folder(layout, 'a').directIndices, [2], 'delete_dir a belongs to the a folder itself');
  assert.deepEqual(folder(layout, 'a/b').directIndices, [0, 1], 'delete_dir a/b and its child share the a/b subtree');
  assert.deepEqual(folder(layout, 'a').children.map((node) => node.path), ['a/b']);
  assert.deepEqual(members(layout, 'a'), [2, 0, 1]);
  assert.deepEqual(members(layout, 'a/b'), [0, 1]);
  assert.equal(folder(layout, 'a').executableCount, 3);
  assert.equal(folder(layout, 'a/b').executableCount, 2);
  assertPermutation(layout.displayOrder, [0, 1, 2]);
});
