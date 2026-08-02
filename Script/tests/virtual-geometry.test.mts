import assert from 'node:assert/strict';
import test from 'node:test';

import {
  maximumPhysicalBodyPixels,
  projectLogicalScrollTop,
  projectVirtualGeometry,
} from '#ui/features/compare-results/model/virtualGeometry.ts';

test('logical and physical scroll positions round-trip for an uncapped result', () => {
  const input = {
    logicalBodyHeight: 12_000,
    headHeight: 42,
    viewportHeight: 700,
    physicalScrollTop: 4_321,
  };
  const projected = projectVirtualGeometry(input);
  assert.equal(projected.physicalScrollTop, input.physicalScrollTop);
  assert.equal(projected.logicalScrollTop, input.physicalScrollTop);
  assert.equal(projectLogicalScrollTop({
    ...input,
    logicalScrollTop: projected.logicalScrollTop,
  }), input.physicalScrollTop);
});

test('logical ownership survives the bounded physical canvas projection', () => {
  const logicalBodyHeight = maximumPhysicalBodyPixels * 8;
  const input = {
    logicalBodyHeight,
    headHeight: 40,
    viewportHeight: 900,
    physicalScrollTop: 900_000,
  };
  const projected = projectVirtualGeometry(input);
  assert.equal(projected.canvasHeight, maximumPhysicalBodyPixels + input.headHeight);
  assert.ok(projected.logicalScrollTop > projected.physicalScrollTop);
  const restored = projectLogicalScrollTop({
    logicalBodyHeight,
    headHeight: input.headHeight,
    viewportHeight: input.viewportHeight,
    logicalScrollTop: projected.logicalScrollTop,
  });
  assert.ok(Math.abs(restored - input.physicalScrollTop) < 0.000_001);
});

test('both projections clamp invalid and out-of-range positions', () => {
  const dimensions = { logicalBodyHeight: 1_000, headHeight: 40, viewportHeight: 400 };
  assert.equal(projectVirtualGeometry({ ...dimensions, physicalScrollTop: -50 }).logicalScrollTop, 0);
  assert.equal(projectLogicalScrollTop({ ...dimensions, logicalScrollTop: -50 }), 0);
  assert.equal(
    projectLogicalScrollTop({ ...dimensions, logicalScrollTop: 50_000 }),
    dimensions.logicalBodyHeight + dimensions.headHeight - dimensions.viewportHeight,
  );
});
