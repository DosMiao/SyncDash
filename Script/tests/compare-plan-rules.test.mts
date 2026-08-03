import assert from 'node:assert/strict';
import test from 'node:test';

import {
  COMPARE_PLAN_RULE_VECTORS,
  MTIME_WINDOW_FLOOR_MS,
  type ComparePlanRuleVector,
} from '#core/types/generated/comparePlanRuleVectors.ts';
import {
  canReverseOperation,
  effectiveOperation,
  rowMetadata,
  sidePaths,
  sortValue,
  type PlanDto,
  type PlanOperation,
} from '#core/domain/compare/plan.ts';

/// Reversal, per-side path derivation, elided-metadata reconstruction, and the action rank exist
/// once in `pipeline::compare::evidence` and once in `core/domain/compare/plan.ts`, because the
/// window cannot ask the backend per click for a direction toggle or per keystroke for a six-figure
/// table. Rust owns all four; these vectors are Rust's answers, emitted by
/// `Dev/src/workflow/pipeline/compare/tests/rule_vectors.rs` through `npm run gen:types`.
///
/// The channel this protects is silent in both directions. Apply sends `{index,
/// direction_reversed}` and Rust reconstructs the executed operation itself, so a frontend that
/// reversed a row differently would show the operator one operation, total its bytes, and execute
/// another — with nothing between the two to notice. Regenerate with `npm run gen:types`;
/// `npm run gen:types:check` fails when the committed vectors no longer match the engine.

const OPTIONAL_OPERATION_FIELDS = ['from', 'size', 'mtime_ms', 'hash', 'link', 'mode'] as const;

/// Rust omits an absent optional field, the TypeScript copy writes an explicit null. Same value in
/// two spellings, so both sides are read through one.
function normalizedOperation(operation: PlanOperation): Record<string, unknown> {
  const normalized: Record<string, unknown> = {
    side: operation.side,
    action: operation.action,
    path: operation.path,
    reason: operation.reason,
  };
  for (const field of OPTIONAL_OPERATION_FIELDS) {
    normalized[field] = operation[field] ?? null;
  }
  return normalized;
}

function singleRowPlan(vector: ComparePlanRuleVector, metas: PlanDto['metas']): PlanDto {
  return {
    owner: {} as PlanDto['owner'],
    header: {} as PlanDto['header'],
    ops: [vector.op],
    metas,
    identical_count: 0,
    identical_bytes: 0,
    mtime_window_ms: MTIME_WINDOW_FLOOR_MS,
  };
}

test('the compare-plan rules the window re-derives answer exactly as the engine does', () => {
  assert.ok(COMPARE_PLAN_RULE_VECTORS.length > 0, 'the generated rule vectors must be present');

  for (const vector of COMPARE_PLAN_RULE_VECTORS) {
    const plan = singleRowPlan(vector, [vector.meta]);

    assert.deepEqual(sidePaths(vector.op), vector.side_paths, `${vector.name}: side paths`);
    assert.deepEqual(
      rowMetadata(singleRowPlan(vector, [null]), 0),
      vector.reconstructed_meta,
      `${vector.name}: metadata reconstructed from an elided entry`,
    );
    assert.equal(
      sortValue(plan, [false], 0, 'action')[1],
      vector.action_rank,
      `${vector.name}: action rank`,
    );
    assert.equal(
      normalizedOperation(effectiveOperation(plan, [false], 0)).path,
      vector.op.path,
      `${vector.name}: an unreversed row is the plan's own row`,
    );
    assert.equal(
      canReverseOperation(plan, 0),
      vector.reversed !== null,
      `${vector.name}: reversibility`,
    );

    if (vector.reversed === null) {
      // The engine refuses; falling back to the forward operation would offer the operator the
      // opposite direction from the one they asked for.
      assert.throws(
        () => effectiveOperation(plan, [true], 0),
        /cannot be reversed/,
        `${vector.name}: an unreversible row must refuse rather than answer with the forward row`,
      );
      continue;
    }

    assert.deepEqual(
      normalizedOperation(effectiveOperation(plan, [true], 0)),
      normalizedOperation(vector.reversed),
      `${vector.name}: reversal`,
    );
    assert.deepEqual(
      sidePaths(effectiveOperation(plan, [true], 0)),
      vector.reversed_side_paths,
      `${vector.name}: side paths of the reversed row`,
    );
  }
});

test('the generated vectors still carry the rows that are hard to keep aligned', () => {
  const named = new Map(COMPARE_PLAN_RULE_VECTORS.map((vector) => [vector.name, vector]));
  const required: [string, (vector: ComparePlanRuleVector) => boolean, string][] = [
    // One `Op` carries one path, so a row whose two sides are spelled differently has to derive
    // both from it — and a reversed Update takes its size from the side it now writes from.
    ['update/target/renamed/both_sides', (vector) => vector.reversed?.size === 22, 'reverses onto the measured origin'],
    // The new origin was never measured: a sizeless write row reads as zero bytes to the
    // free-space gate, so the engine refuses the reversal outright.
    ['update/target/renamed/source_only', (vector) => vector.reversed === null, 'refuses an unmeasured origin'],
    // A link publishes no content and is measured on neither side, so it stays sizeless.
    ['update/source/symlink/unmeasured', (vector) => vector.reversed?.link != null && vector.reversed.size == null, 'keeps the link and no size'],
    ['delete/target/measured/unmeasured', (vector) => vector.reversed?.action === 'copy', 'a delete reverses into a copy'],
    ['conflict/source/measured/both_sides', (vector) => vector.reversed === null, 'a report never reverses'],
    // No counterpart on the other side: this is the row whose `metas` entry is elided on the wire.
    ['copy/target/measured/unmeasured', (vector) => vector.reconstructed_meta.src !== null && vector.reconstructed_meta.dst === null, 'reconstructs its sole side'],
    // The row the table renders as "(this folder)" — the shortening is the window's alone, the
    // derivation it shortens is here.
    ['delete_dir/target/folder/unmeasured', (vector) => vector.side_paths[0] === null, 'exists on one side only'],
  ];

  for (const [name, holds, description] of required) {
    const vector = named.get(name);
    assert.ok(vector, `${name} is missing from the generated vectors`);
    assert.ok(holds(vector), `${name} no longer ${description}`);
  }
});
