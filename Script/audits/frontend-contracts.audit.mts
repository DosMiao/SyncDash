import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import ts from 'typescript';

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function tsxFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return tsxFiles(path);
    return entry.isFile() && entry.name.endsWith('.tsx') ? [path] : [];
  }));
  return nested.flat();
}

test('React source has no inline styles and every native button declares its type', async () => {
  const roots = [
    join(repositoryRoot, 'typescript/ui'),
    join(repositoryRoot, 'typescript/progress'),
  ];
  const files = (await Promise.all(roots.map(tsxFiles))).flat();
  assert.ok(files.length > 0, 'React source files must be discoverable');

  for (const path of files) {
    const contents = await readFile(path, 'utf8');
    const parsed = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
    const inspect = (node: ts.Node) => {
      if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
        const line = parsed.getLineAndCharacterOfPosition(node.pos).line + 1;
        const attributes = node.attributes.properties.filter(ts.isJsxAttribute);
        assert.equal(
          attributes.some((attribute) => attribute.name.getText(parsed) === 'style'),
          false,
          `${path}:${line} has a CSP-blocked style attribute`,
        );
        if (node.tagName.getText(parsed) === 'button') {
          assert.equal(
            attributes.some((attribute) => attribute.name.getText(parsed) === 'type'),
            true,
            `${path}:${line} lacks a native button type`,
          );
        }
      }
      ts.forEachChild(node, inspect);
    };
    inspect(parsed);
  }
});
