import assert from 'node:assert/strict';
import test from 'node:test';

import { newerSide, type PlanDto, type PlanOperation } from '#core/domain/compare/plan.ts';

/// The mtime equality window is a per-run measurement, not a constant. `pipeline::compare::policy`
/// supplies a floor, and `run::local::compare` widens it to the coarser of the two backends'
/// declared mtime precision before the comparison is made — an FTP LIST root thinks in minutes.
/// The result publishes the window it actually used as `PlanDto.mtime_window_ms`, and this suite
/// holds the window's one presentation consumer to that number: a pair of timestamps the engine
/// treated as the same instant must never be painted as one side being newer.

function planWithWindow(
  mtimeWindowMs: number,
  sourceMtimeMs: number,
  targetMtimeMs: number,
): PlanDto {
  const operation: PlanOperation = {
    side: 'target',
    action: 'chmod',
    path: 'scripts/run.sh',
    from: null,
    size: 128,
    mtime_ms: sourceMtimeMs,
    hash: null,
    link: null,
    mode: 0o755,
    reason: 'permission bits differ',
  };
  return {
    owner: {} as PlanDto['owner'],
    header: {} as PlanDto['header'],
    ops: [operation],
    metas: [{
      src: { size: 128, mtime_ms: sourceMtimeMs },
      dst: { size: 128, mtime_ms: targetMtimeMs },
    }],
    identical_count: 0,
    identical_bytes: 0,
    mtime_window_ms: mtimeWindowMs,
  };
}

test('the newer-side cue reads the window this comparison used, not a fixed default', () => {
  // A local pair: the policy floor is the effective window, and it still separates a real edit
  // from FAT/SMB rounding noise.
  assert.equal(newerSide(planWithWindow(2_000, 12_000, 10_000), 0), '',
    'a difference exactly at the window is the same instant');
  assert.equal(newerSide(planWithWindow(2_000, 12_500, 10_000), 0), 's',
    'a difference past the window names the newer side');
  assert.equal(newerSide(planWithWindow(2_000, 10_000, 12_500), 0), 't');

  // A coarse-timestamp backend: `run::local::compare` widened the window to the backend's
  // precision, so the engine already ruled these two timestamps the same instant.
  assert.equal(newerSide(planWithWindow(60_000, 40_000, 10_000), 0), '',
    'a row the engine judged equal must not be painted newer');
  assert.equal(newerSide(planWithWindow(60_000, 10_000, 40_000), 0), '');
  assert.equal(newerSide(planWithWindow(60_000, 71_000, 10_000), 0), 's',
    'past the widened window the cue still appears');
});

test('an unmeasured side never claims a newer timestamp', () => {
  const plan = planWithWindow(2_000, 12_500, 10_000);
  const partial: PlanDto = { ...plan, metas: [{ src: plan.metas[0]!.src, dst: null }] };
  assert.equal(newerSide(partial, 0), '');
});
