export type RootField = 'source' | 'target';
export type RootEditorKey = string & { readonly __rootEditorKey: unique symbol };

export interface RootEditorOwner {
  jobId: string;
  jobName: string;
  configRevision: string;
  targetIndex: number;
}

export interface RootValues {
  source: string;
  target: string;
}

export interface RootFieldConflict {
  previousCommittedValue: string;
  currentCommittedValue: string;
}

export type RootSaveState =
  | { status: 'idle'; requestId: number }
  | {
    status: 'saving';
    requestId: number;
    field: RootField;
  }
  | { status: 'failed'; requestId: number; field: RootField; error: string };

export interface RootEditorWorkspace {
  key: RootEditorKey;
  owner: RootEditorOwner;
  committed: RootValues;
  draft: RootValues;
  conflicts: Partial<Record<RootField, RootFieldConflict>>;
  save: RootSaveState;
}

export interface RootEditorRepository {
  activeKey: RootEditorKey | null;
  workspaces: RootEditorWorkspace[];
}

export type RootEditorAction =
  | { type: 'selection_rebound'; owner: RootEditorOwner | null; values: RootValues }
  | { type: 'draft_changed'; workspaceKey: RootEditorKey; field: RootField; value: string }
  | { type: 'draft_reverted'; workspaceKey: RootEditorKey; field: RootField }
  | { type: 'draft_conflict_accepted'; workspaceKey: RootEditorKey; field: RootField }
  | {
    type: 'workspace_rebound';
    workspaceKey: RootEditorKey;
    owner: RootEditorOwner;
    values: RootValues;
  }
  | { type: 'save_started'; workspaceKey: RootEditorKey; requestId: number; field: RootField }
  | {
    type: 'save_committed';
    workspaceKey: RootEditorKey;
    requestId: number;
    owner: RootEditorOwner;
    values: RootValues;
  }
  | { type: 'save_failed'; workspaceKey: RootEditorKey; requestId: number; error: string }
  | { type: 'job_removed'; jobId: string };

export const emptyRootEditorRepository: RootEditorRepository = {
  activeKey: null,
  workspaces: [],
};

function rootEditorKey(owner: Pick<RootEditorOwner, 'jobId' | 'targetIndex'>): RootEditorKey {
  return JSON.stringify([owner.jobId, owner.targetIndex]) as RootEditorKey;
}

export function activeRootEditor(repository: RootEditorRepository): RootEditorWorkspace | null {
  return repository.workspaces.find((workspace) => workspace.key === repository.activeKey) ?? null;
}

export function rootDraftIsDirty(workspace: RootEditorWorkspace, field: RootField): boolean {
  return workspace.draft[field].trim() !== workspace.committed[field];
}

function replaceWorkspace(
  repository: RootEditorRepository,
  workspaceKey: RootEditorKey,
  update: (workspace: RootEditorWorkspace) => RootEditorWorkspace,
): RootEditorRepository {
  let changed = false;
  const workspaces = repository.workspaces.map((workspace) => {
    if (workspace.key !== workspaceKey) return workspace;
    const next = update(workspace);
    changed ||= next !== workspace;
    return next;
  });
  return changed ? { ...repository, workspaces } : repository;
}

function rebindWorkspace(
  workspace: RootEditorWorkspace,
  owner: RootEditorOwner,
  values: RootValues,
): RootEditorWorkspace {
  const nextDraft = { ...workspace.draft };
  const conflicts = { ...workspace.conflicts };
  for (const field of ['source', 'target'] as const) {
    const draftWasDirty = rootDraftIsDirty(workspace, field);
    if (!draftWasDirty) {
      nextDraft[field] = values[field];
      delete conflicts[field];
    } else if (workspace.draft[field].trim() === values[field]) {
      nextDraft[field] = values[field];
      delete conflicts[field];
    } else if (workspace.committed[field] !== values[field]) {
      conflicts[field] = {
        previousCommittedValue: workspace.committed[field],
        currentCommittedValue: values[field],
      };
    }
  }
  let save = workspace.save;
  if (save.status === 'saving' && workspace.owner.configRevision !== owner.configRevision) {
    save = {
      status: 'failed',
      requestId: save.requestId,
      field: save.field,
      error: 'The job changed while this root save was in flight; review the latest value before retrying',
    };
  }
  return {
    ...workspace,
    owner,
    committed: values,
    draft: nextDraft,
    conflicts,
    save,
  };
}

export function reduceRootEditors(
  repository: RootEditorRepository,
  action: RootEditorAction,
): RootEditorRepository {
  switch (action.type) {
    case 'selection_rebound': {
      if (!action.owner) {
        return repository.activeKey === null ? repository : { ...repository, activeKey: null };
      }
      const key = rootEditorKey(action.owner);
      const existing = repository.workspaces.find((workspace) => workspace.key === key);
      if (!existing) {
        return {
          activeKey: key,
          workspaces: [...repository.workspaces, {
            key,
            owner: action.owner,
            committed: action.values,
            draft: action.values,
            conflicts: {},
            save: { status: 'idle', requestId: 0 },
          }],
        };
      }
      const rebound = rebindWorkspace(existing, action.owner, action.values);
      return {
        activeKey: key,
        workspaces: repository.workspaces.map((workspace) => (
          workspace.key === key ? rebound : workspace
        )),
      };
    }
    case 'draft_changed':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => {
        if (workspace.save.status === 'saving' || workspace.draft[action.field] === action.value) {
          return workspace;
        }
        return {
          ...workspace,
          draft: { ...workspace.draft, [action.field]: action.value },
          save: { status: 'idle', requestId: workspace.save.requestId },
        };
      });
    case 'draft_reverted':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => {
        if (workspace.save.status === 'saving') return workspace;
        const conflicts = { ...workspace.conflicts };
        delete conflicts[action.field];
        return {
          ...workspace,
          draft: { ...workspace.draft, [action.field]: workspace.committed[action.field] },
          conflicts,
          save: { status: 'idle', requestId: workspace.save.requestId },
        };
      });
    case 'draft_conflict_accepted':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => {
        if (workspace.save.status === 'saving' || !workspace.conflicts[action.field]) return workspace;
        const conflicts = { ...workspace.conflicts };
        delete conflicts[action.field];
        return { ...workspace, conflicts };
      });
    case 'workspace_rebound':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => (
        rebindWorkspace(workspace, action.owner, action.values)
      ));
    case 'save_started':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => {
        if (workspace.save.status === 'saving'
          || action.requestId <= workspace.save.requestId
          || !rootDraftIsDirty(workspace, action.field)
          || !workspace.draft[action.field].trim()
          || workspace.conflicts[action.field]) return workspace;
        return {
          ...workspace,
          save: {
            status: 'saving',
            requestId: action.requestId,
            field: action.field,
          },
        };
      });
    case 'save_committed':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => (
        workspace.save.status !== 'saving' || workspace.save.requestId !== action.requestId
          ? workspace
          : {
            ...workspace,
            owner: action.owner,
            committed: action.values,
            draft: action.values,
            conflicts: {},
            save: { status: 'idle', requestId: action.requestId },
          }
      ));
    case 'save_failed':
      return replaceWorkspace(repository, action.workspaceKey, (workspace) => {
        if (workspace.save.status !== 'saving' || workspace.save.requestId !== action.requestId) {
          return workspace;
        }
        const field = workspace.save.field;
        return {
          ...workspace,
          save: {
            status: 'failed',
            requestId: action.requestId,
            field,
            error: action.error,
          },
        };
      });
    case 'job_removed': {
      const workspaces = repository.workspaces.filter((workspace) => workspace.owner.jobId !== action.jobId);
      if (workspaces.length === repository.workspaces.length) return repository;
      return {
        activeKey: workspaces.some((workspace) => workspace.key === repository.activeKey)
          ? repository.activeKey
          : null,
        workspaces,
      };
    }
  }
}
