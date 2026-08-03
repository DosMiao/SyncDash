import { preferenceErrorMessage, type PreferenceStorage } from './preferenceStorage.ts';

export interface PathHistoryLoad {
  paths: string[];
  warning: string | null;
}

const PATH_HISTORY_KEY = 'sd.path-history.v1';
const MAX_PATH_HISTORY = 12;

function validatedPaths(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.some((path) => typeof path !== 'string')) return null;
  const unique = new Set<string>();
  const paths: string[] = [];
  for (const rawPath of value) {
    const path = rawPath.trim();
    const identity = path.toLowerCase();
    if (!path || unique.has(identity)) continue;
    unique.add(identity);
    paths.push(path);
    if (paths.length === MAX_PATH_HISTORY) break;
  }
  return paths;
}

export function loadPathHistory(storage: PreferenceStorage): PathHistoryLoad {
  try {
    const raw = storage.getItem(PATH_HISTORY_KEY);
    if (raw === null) return { paths: [], warning: null };
    const paths = validatedPaths(JSON.parse(raw));
    if (!paths) {
      return {
        paths: [],
        warning: 'Saved path history was invalid and was not used.',
      };
    }
    return { paths, warning: null };
  } catch (error) {
    return {
      paths: [],
      warning: `Path history could not be loaded: ${preferenceErrorMessage(error)}`,
    };
  }
}

export function addPathToHistory(paths: string[], candidate: string): string[] {
  const path = candidate.trim();
  if (!path) return paths;
  return [
    path,
    ...paths.filter((existing) => existing.toLowerCase() !== path.toLowerCase()),
  ].slice(0, MAX_PATH_HISTORY);
}

export function savePathHistory(storage: PreferenceStorage, paths: string[]): string | null {
  try {
    storage.setItem(PATH_HISTORY_KEY, JSON.stringify(paths));
    return null;
  } catch (error) {
    return `Path history could not be saved: ${preferenceErrorMessage(error)}`;
  }
}
