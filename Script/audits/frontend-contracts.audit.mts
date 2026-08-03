import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

import { filesUnder, repositoryRoot } from './tree.mts';

const isReactSource = (name: string) => name.endsWith('.tsx');
const isFrontendSource = (name: string) => /\.tsx?$/.test(name);

test('React source has no inline styles and every native button declares its type', async () => {
  const roots = [join(repositoryRoot, 'Dev/typescript/ui')];
  const files = (await Promise.all(roots.map((root) => filesUnder(root, isReactSource)))).flat();
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

function staticImports(path: string, contents: string): string[] {
  const kind = path.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
  const parsed = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, kind);
  const imports: string[] = [];
  for (const statement of parsed.statements) {
    if ((ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement))
      && statement.moduleSpecifier
      && ts.isStringLiteral(statement.moduleSpecifier)
    ) {
      imports.push(statement.moduleSpecifier.text);
    }
  }
  return imports;
}

function repositoryTarget(sourcePath: string, specifier: string): string | null {
  let absolutePath: string;
  if (specifier.startsWith('.')) {
    absolutePath = resolve(dirname(sourcePath), specifier);
  } else if (specifier.startsWith('#core/')) {
    absolutePath = join(repositoryRoot, 'Dev/typescript/core', specifier.slice('#core/'.length));
  } else if (specifier.startsWith('#ui/')) {
    absolutePath = join(repositoryRoot, 'Dev/typescript/ui', specifier.slice('#ui/'.length));
  } else {
    return null;
  }
  return relative(repositoryRoot, absolutePath).replaceAll('\\', '/');
}

test('frontend dependency layers follow the Dev tree', async () => {
  const frontendRoot = join(repositoryRoot, 'Dev/typescript');
  const files = await filesUnder(frontendRoot, isFrontendSource);
  assert.ok(files.length > 0, 'TypeScript source files must be discoverable');

  for (const path of files) {
    const repositoryPath = relative(repositoryRoot, path).replaceAll('\\', '/');
    const imports = staticImports(path, await readFile(path, 'utf8'));
    const edges = imports.map((specifier) => ({
      specifier,
      target: repositoryTarget(path, specifier),
    }));

    for (const specifier of imports.filter((value) => value.startsWith('@tauri-apps/'))) {
      assert.match(
        repositoryPath,
        /^Dev\/typescript\/core\/infrastructure\/tauri\//,
        `${repositoryPath} imports ${specifier} outside the Tauri infrastructure boundary`,
      );
    }

    for (const { specifier, target } of edges) {
      if (!target) continue;


      // Window authority: each window may reach only its own command surface. The comment on
      // commands/main.ts used to be the only statement of this rule, which meant deleting that
      // file would have deleted the rule.
      const PROGRESS_ONLY = 'Dev/typescript/core/infrastructure/tauri/commands/progress.ts';
      const MAIN_ONLY = /^Dev\/typescript\/core\/infrastructure\/tauri\/commands\/(?:apply|autoscan|compare|jobs|logs|operations|paths|settings)\.ts$/;
      const inProgressWindow = /^Dev\/typescript\/ui\/(?:pages|windows)\/progress\//.test(repositoryPath);
      const inWorkspaceWindow = /^Dev\/typescript\/ui\/(?:pages|windows)\/(?:workspace|main)\//.test(repositoryPath)
        || repositoryPath.startsWith('Dev/typescript/ui/features/');

      if (inWorkspaceWindow) {
        assert.notEqual(
          target,
          PROGRESS_ONLY,
          `${repositoryPath} reaches progress-window authority through ${specifier}`,
        );
      }
      if (inProgressWindow) {
        assert.doesNotMatch(
          target,
          MAIN_ONLY,
          `${repositoryPath} reaches main-window command authority through ${specifier}`,
        );
      }

      if (repositoryPath.startsWith('Dev/typescript/core/domain/')) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/(?:core\/(?:application|infrastructure)|ui)\//,
          `${repositoryPath} points pure domain code outward through ${specifier} -> ${target}`,
        );
      }

      if (repositoryPath.startsWith('Dev/typescript/core/application/')) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/(?:core\/infrastructure|ui)\//,
          `${repositoryPath} points application state outward through ${specifier} -> ${target}`,
        );
      }

      if (repositoryPath.startsWith('Dev/typescript/core/infrastructure/')) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/ui\//,
          `${repositoryPath} points infrastructure into the UI through ${specifier} -> ${target}`,
        );
      }

      if (repositoryPath.startsWith('Dev/typescript/ui/shared/')) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/ui\/(?:features|pages|windows)\//,
          `${repositoryPath} points shared UI upward through ${specifier} -> ${target}`,
        );
      }

      const modelBranch = repositoryPath.match(
        /^(Dev\/typescript\/ui\/(?:features\/[^/]+|pages\/[^/]+)\/model\/)/,
      )?.[1];
      if (modelBranch && !target.startsWith(modelBranch)) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/(?:core\/infrastructure|ui\/(?:pages|windows|shared\/(?:hooks|interaction|status)))\//,
          `${repositoryPath} lets a model branch depend on delivery or runtime code through ${specifier} -> ${target}`,
        );
      }

      const componentOwner = repositoryPath.match(
        /^(Dev\/typescript\/ui\/(?:features\/[^/]+|pages\/[^/]+))\/(?:components|presentation)\//,
      )?.[1];
      if (componentOwner) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/core\/infrastructure\//,
          `${repositoryPath} lets rendering reach platform effects through ${specifier} -> ${target}`,
        );
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/ui\/windows\//,
          `${repositoryPath} lets rendering reach window composition through ${specifier} -> ${target}`,
        );
        if (target.startsWith('Dev/typescript/ui/pages/') && !target.startsWith(`${componentOwner}/`)) {
          assert.fail(`${repositoryPath} reaches another page through ${specifier} -> ${target}`);
        }
        assert.doesNotMatch(
          target,
          /\/(?:controller|controllers|runtime)\//,
          `${repositoryPath} lets rendering reach effects or composition through ${specifier} -> ${target}`,
        );
      }

      if (/^Dev\/typescript\/ui\/pages\/[^/]+\/runtime\//.test(repositoryPath)) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/ui\/windows\/|\/(?:controller|controllers|components)\//,
          `${repositoryPath} lets runtime code depend on rendering or composition through ${specifier} -> ${target}`,
        );
      }

      if (/^Dev\/typescript\/ui\/pages\/[^/]+\/controller\//.test(repositoryPath)) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/core\/infrastructure\//,
          `${repositoryPath} bypasses page runtime through ${specifier} -> ${target}`,
        );
      }

      if (repositoryPath.startsWith('Dev/typescript/ui/pages/progress/')) {
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/ui\/(?:features|pages\/workspace|windows\/main)\//,
          `${repositoryPath} couples the independent progress page through ${specifier} -> ${target}`,
        );
      }
    }

    if (repositoryPath.startsWith('Dev/typescript/core/domain/')) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^(?:react|react-dom|@tauri-apps\/|#core\/(?:application|infrastructure)\/|#ui\/)/,
          `${repositoryPath} points pure domain code outward through ${specifier}`,
        );
      }
    }

    if (repositoryPath.startsWith('Dev/typescript/core/application/')) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^(?:react|react-dom|@tauri-apps\/|#core\/infrastructure\/|#ui\/)/,
          `${repositoryPath} points application state outward through ${specifier}`,
        );
      }
    }

    if (repositoryPath.startsWith('Dev/typescript/core/infrastructure/')) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^#ui\//,
          `${repositoryPath} points infrastructure into the UI through ${specifier}`,
        );
      }
    }

    if (repositoryPath.startsWith('Dev/typescript/ui/shared/')) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^#ui\/(?:features|pages|windows)\//,
          `${repositoryPath} points shared UI upward through ${specifier}`,
        );
      }
    }

    const pageRoot = repositoryPath.match(
      /^Dev\/typescript\/ui\/pages\/([^/]+)\/([^/]+Page\.tsx)$/,
    );
    if (pageRoot) {
      const expectedController = `./controller/${pageRoot[2].replace(/Page\.tsx$/, 'PageController.tsx')}`;
      assert.deepEqual(
        imports,
        [expectedController],
        `${repositoryPath} must remain a one-controller page facade`,
      );
    }

    if (/^Dev\/typescript\/ui\/(?:features\/[^/]+|pages\/[^/]+)\/model\//.test(repositoryPath)) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^(?:@tauri-apps\/|#core\/infrastructure\/|#ui\/(?:pages|windows|shared\/(?:hooks|interaction|status))\/)/,
          `${repositoryPath} lets a model branch depend on delivery or runtime code through ${specifier}`,
        );
      }
    }

    if (/^Dev\/typescript\/ui\/(?:features\/[^/]+|pages\/[^/]+)\/components\//.test(repositoryPath)) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^(?:@tauri-apps\/|#core\/infrastructure\/|#ui\/(?:pages|windows)\/|\.\.\/controller\/|\.\.\/runtime\/)/,
          `${repositoryPath} lets rendering reach into effects or composition through ${specifier}`,
        );
      }
    }

    if (/^Dev\/typescript\/ui\/pages\/[^/]+\/runtime\//.test(repositoryPath)) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^(?:#ui\/windows\/|\.\.\/controller\/|\.\.\/components\/)/,
          `${repositoryPath} lets runtime code depend on rendering or composition through ${specifier}`,
        );
      }
    }

    if (repositoryPath.startsWith('Dev/typescript/ui/pages/progress/')) {
      for (const specifier of imports) {
        assert.doesNotMatch(
          specifier,
          /^#ui\/(?:features|pages\/workspace|windows\/main)\//,
          `${repositoryPath} couples the independent progress page to the workspace through ${specifier}`,
        );
      }
    }

    if (/^Dev\/typescript\/ui\/windows\/[^/]+\/[^/]+Window\.tsx$/.test(repositoryPath)) {
      const pageImports = edges.filter(({ target }) => target?.startsWith('Dev/typescript/ui/pages/'));
      assert.equal(
        pageImports.length,
        1,
        `${repositoryPath} must compose one page boundary`,
      );
      for (const { specifier, target } of edges) {
        if (!target) continue;
        assert.doesNotMatch(
          target,
          /^Dev\/typescript\/(?:core\/(?:application|domain|infrastructure)|ui\/(?:features|shared))\//,
          `${repositoryPath} bypasses its page boundary through ${specifier} -> ${target}`,
        );
      }
    }
  }
});
