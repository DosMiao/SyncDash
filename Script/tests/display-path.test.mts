import assert from 'node:assert/strict';
import test from 'node:test';

import { joinDisplayPath } from '#core/shared/format.ts';

/// These cases are the ones Rust `foundation::path::join_display` pins in
/// `display_join_spells_the_far_machine_and_never_doubles_a_separator`. The two implementations
/// render the same row — one into a CSV export, one into the window — so a divergence shows up as
/// the same file having two different absolute paths depending on where the user reads it.
test('display joining matches the Rust arbiter, including repeated trailing separators', () => {
  assert.equal(joinDisplayPath('/mnt/share', 'a/b.txt'), '/mnt/share/a/b.txt');
  assert.equal(
    joinDisplayPath('C:\\root', 'a/b.txt'),
    'C:\\root\\a\\b.txt',
    'a windows-spelled root takes windows separators throughout',
  );

  // Trimming only the last separator produced a doubled one here while the export produced a
  // single one — the drift this pins.
  assert.equal(joinDisplayPath('//server/share//', 'a.txt'), '//server/share/a.txt');
  assert.equal(joinDisplayPath('C:\\root\\\\', 'a.txt'), 'C:\\root\\a.txt');
  assert.equal(joinDisplayPath('/mnt/share/', 'a.txt'), '/mnt/share/a.txt');
});
