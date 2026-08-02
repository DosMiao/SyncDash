interface PathHistoryStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface PathHistoryLoad {
  paths: string[];
  warning: string | null;
}

const PATH_HISTORY_KEY = 'sd.path-history.v1';
const LEGACY_PATH_HISTORY_KEY = 'sd.pathhist';
const MAX_PATH_HISTORY = 12;

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

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

export function loadPathHistory(storage: PathHistoryStorage): PathHistoryLoad {
  try {
    const current = storage.getItem(PATH_HISTORY_KEY);
    const legacy = current === null ? storage.getItem(LEGACY_PATH_HISTORY_KEY) : null;
    const raw = current ?? legacy;
    if (raw === null) return { paths: [], warning: null };
    const paths = validatedPaths(JSON.parse(raw));
    if (!paths) {
      return {
        paths: [],
        warning: 'Saved path history was invalid and was not used.',
      };
    }
    if (legacy !== null) {
      try {
        storage.setItem(PATH_HISTORY_KEY, JSON.stringify(paths));
        storage.removeItem(LEGACY_PATH_HISTORY_KEY);
      } catch (error) {
        return {
          paths,
          warning: `Path history was loaded but could not be migrated: ${errorMessage(error)}`,
        };
      }
    }
    return { paths, warning: null };
  } catch (error) {
    return {
      paths: [],
      warning: `Path history could not be loaded: ${errorMessage(error)}`,
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

export function savePathHistory(storage: PathHistoryStorage, paths: string[]): string | null {
  try {
    storage.setItem(PATH_HISTORY_KEY, JSON.stringify(paths));
    return null;
  } catch (error) {
    return `Path history could not be saved: ${errorMessage(error)}`;
  }
}
