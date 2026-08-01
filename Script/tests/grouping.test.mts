import assert from 'node:assert/strict';
import test from 'node:test';

import { buildLayout, flattenLayout, layoutDirs } from '../../typescript/core/grouping.ts';
import type { FolderNode, PlanLayout, RowSpec } from '../../typescript/core/grouping.ts';
import type { OpDto, PlanDto, Sort } from '../../typescript/core/plan.ts';

function op(path: string, patch: Partial<OpDto> = {}): OpDto {
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

function plan(ops: OpDto[]): PlanDto {
  return {
    // Grouping never reads the header. Keep the fixture focused on the paths and row metadata that
    // are its actual input instead of copying the whole Rust wire header into every frontend test.
    header: {} as PlanDto['header'],
    ops,
    metas: ops.map(() => null),
    equal_count: 0,
    equal_bytes: 0,
  };
}

function layoutOf(p: PlanDto, flipped: boolean[] = [], sort: Sort | null = null): PlanLayout {
  return buildLayout({
    plan: p,
    flipped,
    visible: p.ops.map((_, i) => i),
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

function folder(layout: PlanLayout, dir: string): FolderNode {
  const hits = allFolders(layout.tree).filter((node) => node.dir === dir);
  assert.equal(hits.length, 1, `expected exactly one folder node for ${JSON.stringify(dir)}`);
  return hits[0];
}

function members(layout: PlanLayout, dir: string): number[] {
  const node = folder(layout, dir);
  return layout.order.slice(node.start, node.end);
}

function rowTags(p: PlanDto, rows: RowSpec[]): string[] {
  return rows.map((row) => (
    typeof row === 'number'
      ? `row:${p.ops[row].path}`
      : `folder:${row.dir || '(root)'}@${row.depth}`
  ));
}

function assertPermutation(actual: number[], expected: number[]): void {
  assert.equal(new Set(actual).size, actual.length, 'the DFS order must not duplicate operations');
  assert.deepEqual([...actual].sort((a, b) => a - b), [...expected].sort((a, b) => a - b));
}

test('recursive layout synthesizes every intermediate folder while root files stay a sibling group', () => {
  const p = plan([
    op('root.txt'),
    op('docs/readme.md'),
    op('docs/api/v1/openapi.json'),
    op('docs/api/v2/schema.json'),
    op('src/internal/cache.bin'),
  ]);
  const layout = layoutOf(p);

  assert.deepEqual(layout.tree?.map((node) => node.dir), ['', 'docs', 'src']);
  assert.deepEqual(layoutDirs(layout), [
    '',
    'docs',
    'docs/api',
    'docs/api/v1',
    'docs/api/v2',
    'src',
    'src/internal',
  ]);
  assert.deepEqual(
    allFolders(layout.tree).map((node) => [node.dir, node.depth]),
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
  assert.deepEqual(root.direct, [0], 'the (root) pseudo-folder owns root files only');
  assert.deepEqual(root.children, [], 'top-level real folders are siblings, not children of (root)');
  assert.deepEqual(members(layout, ''), [0]);
  assert.deepEqual(folder(layout, 'docs').direct, [1]);
  assert.deepEqual(folder(layout, 'docs/api').direct, []);
  assert.deepEqual(folder(layout, 'docs/api/v1').direct, [2]);
  assert.deepEqual(folder(layout, 'docs/api/v2').direct, [3]);
  assert.deepEqual(folder(layout, 'src/internal').direct, [4]);
  assertPermutation(layout.order, [0, 1, 2, 3, 4]);

  assert.deepEqual(rowTags(p, flattenLayout(layout, new Set())), [
    'folder:(root)@0',
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
  const p = plan([
    op('root.txt'),
    op('a/own.txt'),
    op('a/b/own.txt'),
    op('a/b/c/leaf.txt'),
    op('z/peer.txt'),
  ]);
  const layout = layoutOf(p);

  assert.deepEqual(rowTags(p, flattenLayout(layout, new Set(['a/b/c']))), [
    'folder:(root)@0', 'row:root.txt',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1', 'row:a/b/own.txt',
    'folder:a/b/c@2',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(p, flattenLayout(layout, new Set(['a/b']))), [
    'folder:(root)@0', 'row:root.txt',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(p, flattenLayout(layout, new Set(['a']))), [
    'folder:(root)@0', 'row:root.txt',
    'folder:a@0',
    'folder:z@0', 'row:z/peer.txt',
  ]);
  assert.deepEqual(rowTags(p, flattenLayout(layout, new Set(['']))), [
    'folder:(root)@0',
    'folder:a@0', 'row:a/own.txt',
    'folder:a/b@1', 'row:a/b/own.txt',
    'folder:a/b/c@2', 'row:a/b/c/leaf.txt',
    'folder:z@0', 'row:z/peer.txt',
  ], 'folding (root) must not fold its top-level folder siblings');
});

test('folder intervals aggregate descendants without copying them into every ancestor', () => {
  const p = plan([
    op('a/own.bin', { size: 2 }),
    op('a/b/child.bin', { action: 'update', size: 3 }),
    op('a/b/report.txt', { action: 'conflict', size: 5 }),
    op('a/b/c/leaf.bin', { action: 'delete', size: 7 }),
    op('sibling/note.txt', { action: 'note', size: 11 }),
  ]);
  const layout = layoutOf(p);
  const a = folder(layout, 'a');
  const ab = folder(layout, 'a/b');
  const abc = folder(layout, 'a/b/c');
  const sibling = folder(layout, 'sibling');

  assert.deepEqual(a.direct, [0]);
  assert.deepEqual(ab.direct, [1, 2]);
  assert.deepEqual(abc.direct, [3]);
  assert.deepEqual(members(layout, 'a'), [0, 1, 2, 3]);
  assert.deepEqual(members(layout, 'a/b'), [1, 2, 3]);
  assert.deepEqual(members(layout, 'a/b/c'), [3]);
  assert.deepEqual(members(layout, 'sibling'), [4]);

  assert.deepEqual(
    [
      [a.count, a.selectable, a.bytes],
      [ab.count, ab.selectable, ab.bytes],
      [abc.count, abc.selectable, abc.bytes],
      [sibling.count, sibling.selectable, sibling.bytes],
    ],
    [
      [4, 3, 17],
      [3, 2, 15],
      [1, 1, 7],
      [1, 0, 11],
    ],
  );
  for (const node of [a, ab, abc, sibling]) {
    assert.equal(node.count, node.end - node.start);
  }
  assert.ok(a.start <= ab.start && ab.end <= a.end, 'child intervals must nest inside their parent');
  assert.ok(ab.start <= abc.start && abc.end <= ab.end, 'deep child intervals must stay nested');
  assert.ok(a.end <= sibling.start, 'sibling intervals must not overlap');
  assert.deepEqual(
    layout.order.slice(a.start, a.end).filter((i) => !['conflict', 'note'].includes(p.ops[i].action)),
    [0, 1, 3],
    'a parent checkbox maps its interval to selectable descendants only',
  );
  assertPermutation(layout.order, [0, 1, 2, 3, 4]);

  const renderedA = flattenLayout(layout, new Set()).find(
    (row): row is Exclude<RowSpec, number> => typeof row !== 'number' && row.dir === 'a',
  );
  assert.ok(renderedA);
  assert.deepEqual(
    {
      start: renderedA.start,
      end: renderedA.end,
      count: renderedA.count,
      selectable: renderedA.selectable,
      bytes: renderedA.bytes,
    },
    { start: a.start, end: a.end, count: 4, selectable: 3, bytes: 17 },
  );
});

test('action sorting can reorder flipped subtrees without changing folder membership', () => {
  const p = plan([
    op('alpha/deep/copy.bin', { action: 'copy', size: 10 }),
    op('beta/deep/delete.bin', { action: 'delete', size: 20 }),
  ]);
  const actionAsc: Sort = { key: 'action', dir: 1 };

  const before = layoutOf(p, [false, false], actionAsc);
  assert.deepEqual(layoutDirs(before), ['alpha', 'alpha/deep', 'beta', 'beta/deep']);
  assert.deepEqual(before.order, [0, 1]);
  assert.deepEqual(members(before, 'alpha'), [0]);
  assert.deepEqual(members(before, 'beta'), [1]);

  // copy -> delete and delete -> copy. The aggregate action rank swaps the two top-level folders,
  // but a display sort must never turn the sorted value into the row's directory identity.
  const after = layoutOf(p, [true, true], actionAsc);
  assert.deepEqual(layoutDirs(after), ['beta', 'beta/deep', 'alpha', 'alpha/deep']);
  assert.deepEqual(after.order, [1, 0]);
  assert.deepEqual(members(after, 'alpha'), [0]);
  assert.deepEqual(members(after, 'beta'), [1]);
  assertPermutation(after.order, [0, 1]);
});

test('a cross-directory move is grouped by its destination path, not by a side-specific sort path', () => {
  const p = plan([
    op('new/deep/moved.txt', {
      side: 'source',
      action: 'move',
      from: 'old/place/moved.txt',
      size: 4,
    }),
    op('middle/other.txt', { size: 2 }),
  ]);
  const layout = layoutOf(p, [false, false], { key: 's.path', dir: 1 });
  const dirs = layoutDirs(layout);

  assert.ok(dirs.includes('new'));
  assert.ok(dirs.includes('new/deep'));
  assert.ok(!dirs.includes('old'));
  assert.ok(!dirs.includes('old/place'));
  assert.deepEqual(folder(layout, 'new/deep').direct, [0]);
  assert.deepEqual(members(layout, 'new'), [0]);
  assertPermutation(layout.order, [0, 1]);
});

test('delete_dir is a distinct self row inside the folder its checkbox controls', () => {
  const p = plan([
    op('a/b', { action: 'delete_dir', size: null }),
    op('a/b/child.txt', { action: 'delete', size: 3 }),
    op('a', { action: 'delete_dir', size: null }),
  ]);
  const layout = layoutOf(p);
  const dirs = layoutDirs(layout);

  assert.equal(dirs.filter((dir) => dir === 'a').length, 1);
  assert.equal(dirs.filter((dir) => dir === 'a/b').length, 1);
  assert.ok(!dirs.includes(''), 'a top-level directory operation is not a root-file row');
  assert.deepEqual(folder(layout, 'a').direct, [2], 'delete_dir a belongs to the a folder itself');
  assert.deepEqual(folder(layout, 'a/b').direct, [0, 1], 'delete_dir a/b and its child share the a/b subtree');
  assert.deepEqual(folder(layout, 'a').children.map((node) => node.dir), ['a/b']);
  assert.deepEqual(members(layout, 'a'), [2, 0, 1]);
  assert.deepEqual(members(layout, 'a/b'), [0, 1]);
  assert.equal(folder(layout, 'a').selectable, 3);
  assert.equal(folder(layout, 'a/b').selectable, 2);
  assertPermutation(layout.order, [0, 1, 2]);
});
