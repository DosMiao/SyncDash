import assert from 'node:assert/strict';
import test from 'node:test';

import { humanSize } from '#core/shared/format.ts';

/// These cases are the ones Rust `foundation::fmt::human_bytes` pins in
/// `bytes_use_binary_units_and_drop_decimals_under_1k`. The same `u64` reaches both formatters —
/// `ApplyOutcome.bytes_copied` is printed by `syncdash history` and rendered again by the desktop
/// Log panel — so a divergence shows up as one byte count spelled two ways.
test('byte rendering matches the Rust arbiter away from ties', () => {
  assert.equal(humanSize(0), '0 B');
  assert.equal(humanSize(938), '938 B');
  assert.equal(humanSize(1024), '1.0 KiB');
  assert.equal(humanSize(1024 * 1024 * 3 / 2), '1.5 MiB');
  assert.equal(humanSize(1024 ** 4), '1.0 TiB');
  // Caps at TiB, never rolls off the end of the unit table.
  assert.equal(humanSize(1024 ** 5), '1024.0 TiB');
});

/// A value of `n / 1024^k` is dyadic and therefore exact in f64, so a tie at one decimal is
/// reachable — but only at `.25` and `.75`, the two dyadic tie fractions. Rust's `{:.1}` rounds the
/// exact binary value half-to-even, which sends `.25` down and `.75` up; `toFixed(1)` breaks both
/// ties away from zero, so it used to spell every `.25` one tenth higher than the CLI did.
test('exact tenths ties round half-to-even, matching Rust {:.1}', () => {
  assert.equal(humanSize(1280), '1.2 KiB', '1.25 KiB rounds to the even tenth');
  assert.equal(humanSize(2304), '2.2 KiB');
  assert.equal(humanSize(10_496), '10.2 KiB');
  assert.equal(humanSize(1_310_720), '1.2 MiB', 'exactly 1.25 MiB');
  assert.equal(humanSize(1_342_177_280), '1.2 GiB', 'exactly 1.25 GiB');
  assert.equal(humanSize(1_374_389_534_720), '1.2 TiB');

  // The other dyadic tie already rounds up under both rules; half-to-even must not drag it down.
  assert.equal(humanSize(1792), '1.8 KiB', '1.75 KiB rounds to the even tenth upward');
  assert.equal(humanSize(1024 * 1024 * 7 / 4), '1.8 MiB');

  // Non-ties are untouched.
  assert.equal(humanSize(1153), '1.1 KiB');
  assert.equal(humanSize(1234_567), '1.2 MiB');
});
