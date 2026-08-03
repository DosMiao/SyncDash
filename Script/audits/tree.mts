// Directory traversal shared by the audit suite. Every audit reads the same source trees, so the
// repository root and the "collect everything under here" walk have one owner rather than one copy
// per audit file.

import type { Dirent } from 'node:fs';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The repository root every audit resolves its paths against. */
export const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url));

async function entriesUnder(root: string): Promise<Dirent[]> {
  return readdir(root, { recursive: true, withFileTypes: true });
}

function absolutePath(entry: Dirent): string {
  return join(entry.parentPath, entry.name);
}

/** Every directory below `root`, at any depth. */
export async function directoriesUnder(root: string): Promise<string[]> {
  return (await entriesUnder(root)).filter((entry) => entry.isDirectory()).map(absolutePath);
}

/** Every file below `root`, at any depth, whose base name the caller accepts. */
export async function filesUnder(
  root: string,
  matches: (name: string) => boolean,
): Promise<string[]> {
  return (await entriesUnder(root))
    .filter((entry) => entry.isFile() && matches(entry.name))
    .map(absolutePath);
}

/**
 * The concatenated contents of `filesUnder(root, matches)`, for the assertions that search a whole
 * tree for a forbidden pattern rather than reporting which file carries it.
 */
export async function textUnder(
  root: string,
  matches: (name: string) => boolean,
): Promise<string> {
  const paths = await filesUnder(root, matches);
  const contents = await Promise.all(paths.map((path) => readFile(path, 'utf8')));
  return contents.join('\n');
}
