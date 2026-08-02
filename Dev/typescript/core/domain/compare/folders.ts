import { parentRelativePath } from '#core/shared/format.ts';
import type { PlanOperation } from './plan.ts';

export const ROOT_FOLDER_PATH = '';
export const ROOT_LEVEL_LABEL = 'Root Level';

/// A directory deletion belongs to the directory it targets; every other operation belongs to
/// its parent. Run-scope membership and display grouping must use this exact ownership rule.
export function owningFolderOf(operation: PlanOperation): string {
  return operation.action === 'delete_dir' ? operation.path : parentRelativePath(operation.path);
}
