import assert from 'node:assert/strict';
import test from 'node:test';

import { configPills } from '#ui/features/roots/model/configPills.ts';
import type { Job as JobFull } from '#core/types/generated/Job.ts';
import type { PeerLinkDto } from '#core/types/generated/PeerLinkDto.ts';

const peerJob = (target: string): JobFull => ({
  schema: 6,
  job_id: 'j1',
  mode: 'mirror',
  source: '/Users/ben/Code',
  targets: [target],
  archive: null,
  include: [],
  exclude: [],
  rigor: 'balanced',
  evidence: null,
  use_cache: null,
  escalate: null,
  verify_writes: null,
  case_sensitive: false,
  symlinks: 'exclude',
  versioning: false,
  require_marker: false,
  min_free_pct: 0.01,
  max_delete_ratio: 0.5,
  fsync: true,
  on_conflict: 'report',
  max_conflicts: 5,
  sync_mode: false,
  deletable: [],
  delta: false,
  parallel: null,
  autoscan_interval_secs: null,
  autoscan_auto_apply: false,
});

const peerPill = (target: string, peerLink: PeerLinkDto | null) =>
  configPills(peerJob(target), peerLink, []).find((pill) => pill.key === 'Peer');

/// Each case pairs a stored phrase with the verdict the engine derives for it, and the pill must
/// state that verdict rather than re-read the phrase. These are the three shapes where the two
/// disagree: `spec::parse` lowercases the scheme before testing it and trims option keys and
/// values, and both mount readers discard an empty value. Rust
/// `the_pull_mount_follows_the_parsed_phrase_rather_than_its_spelling` pins the same three phrases
/// on the deriving side, so the pair proves the whole path.
test('the Peer pill states the engine verdict, not a re-reading of the phrase', () => {
  assert.equal(
    peerPill('PEER://mac/Users/ben/Code|mount=/Volumes/peer', { pull_mount: '/Volumes/peer' })?.value,
    'push + pull',
    'the scheme is case-insensitive, so this is a peer job with a working pull mount',
  );
  assert.equal(
    peerPill(
      'peer://mac/Users/ben/Code|exe=~/bin/syncdash| mount = /Volumes/peer',
      { pull_mount: '/Volumes/peer' },
    )?.value,
    'push + pull',
    'option keys and values are trimmed, so a spaced mount is a mount',
  );
  assert.equal(
    peerPill('peer://mac/Users/ben/Code|mount=', { pull_mount: null })?.value,
    'push only',
    'an empty value declares no mount, and Apply refuses every source-side op without one',
  );
  assert.equal(
    peerPill('peer://mac/Users/ben/Code', { pull_mount: null })?.value,
    'push only',
  );
});

test('a job the router does not route to a peer has no Peer pill', () => {
  assert.equal(peerPill('/Volumes/backup', null), undefined);
  assert.equal(peerPill('sftp://host/srv|mount=/Volumes/peer', null), undefined);
});
