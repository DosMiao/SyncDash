import {
  ArrowLeftRight,
  MoveRight,
  Pencil,
  Plus,
  RefreshCw,
  ScrollText,
  Sigma,
  SquarePen,
  Timer,
  Trash2,
  Zap,
} from 'lucide-react';
import { ED_FIELDS, MODE_HINT, RIGOR_HINT, groupsOf } from '../../core/jobfields';
import { humanSize } from '../../core/format';
import type { ReactNode } from 'react';
import type { JobDto } from '../../core/types/generated/JobDto';

/// Edit job opens on the first section; the config pills below name their own
const EDIT_ENTRY = groupsOf(ED_FIELDS)[0];

export interface PlanStats {
  copy: number;
  upd: number;
  mv: number;
  del: number;
  conflicts: number;
  bytes: number;
  flips: number;
}

interface Props {
  job: JobDto | null;
  hasPlan: boolean;
  /// Count of what would actually run (checked ∩ visible) — the same set the confirm sheet totals
  finalCount: number;
  stats: PlanStats | null;
  busy: boolean;
  canSync: boolean;
  watchSecs: number | null;
  onCompare: () => void;
  onSync: () => void;
  onEditGroup: (group: string) => void;
  onToggleLog: () => void;
  onToggleWatch: () => void;
}

/// One count and its icon (same semantics as the FFS bottom bar). A zero recedes by colour rather
/// than alpha — it still has to be readable to say "nothing of this kind", which is itself an answer.
function Seg({ cls, icon, n, title }: { cls?: string; icon: ReactNode; n: number | string; title: string }) {
  const zero = n === 0 || n === '0 B';
  return (
    <span className={'st' + (cls ? ' ' + cls : '') + (zero ? ' zero' : '')} title={title}>
      {icon}<b>{n}</b>
    </span>
  );
}

export function Toolbar(props: Props) {
  const { job, hasPlan, finalCount, stats, busy, canSync, watchSecs, onCompare, onSync, onEditGroup, onToggleLog, onToggleWatch } = props;

  // An unknown tier shows just its name, with no dangling "·" (the rigor ladder will gain tiers later)
  const rh = job ? RIGOR_HINT[job.rigor] : undefined;
  const cmpVariant = job ? (rh ? `${job.rigor} · ${rh}` : job.rigor) : 'Select a job';

  return (
    <header className="toolbar">
      {/* Five controls, every one labelled, every one the same height, no icon used twice.
          What each button *does* is its label; what it will do this time — the rigor tier, the
          item count, the conflict policy — is in `title`. The counts are already on the stats bar
          two inches to the right, so a subtitle repeating them bought nothing and cost the most
          valuable strip on the screen. */}
      <button
        className="btn primary"
        disabled={busy || !job}
        title={job ? `Walk both roots and build a plan (F5).\nRigor: ${cmpVariant}` : 'Select a job first'}
        onClick={onCompare}
      ><RefreshCw size={13} /> Compare</button>

      {/* The label is the **mode**, not a verb: what decides "what will happen" is mirror vs sync
          vs enrich, and the verb never changes. Writing "Synchronize" would also collide the
          action with the mode of the same name — and mirror is not synchronization at all. */}
      <button
        className="btn accent mode-btn"
        disabled={busy || !canSync}
        title={job
          ? `${job.mode}: ${MODE_HINT[job.mode] ?? ''}${job.versioning ? ' · versioning on' : ''}`
            + `${hasPlan ? `\nRuns ${finalCount} checked items (F9)` : '\nCompare first'}`
          : undefined}
        onClick={onSync}
      >{job ? job.mode.toUpperCase() : 'No job'}</button>

      <button
        className="btn"
        title="Open this job's settings"
        disabled={!job}
        onClick={() => onEditGroup(EDIT_ENTRY)}
      ><SquarePen size={13} /> Edit job</button>

      <button className="btn" title="Show the run log" onClick={onToggleLog}>
        <ScrollText size={13} /> Log
      </button>

      {/* A solid fill rather than the usual tinted toggle: Watch is the one control here that keeps
          working after you look away, so it should be legible from across the room */}
      <button
        className={'btn' + (watchSecs !== null ? ' on-solid' : '')}
        title="Compare automatically on the job's watch interval"
        disabled={!job}
        onClick={onToggleWatch}
      >
        <Timer size={13} />
        {watchSecs !== null ? `Watch ${watchSecs}s` : 'Watch'}
      </button>

      <span className="stats">
        {stats && (
          <>
            <Seg cls="s-copy" icon={<Plus size={12} />} n={stats.copy} title="Copy" />
            <Seg cls="s-upd" icon={<Pencil size={12} />} n={stats.upd} title="Update" />
            <Seg cls="s-mv" icon={<MoveRight size={12} />} n={stats.mv} title="Move (no re-transfer)" />
            <Seg cls="s-del" icon={<Trash2 size={12} />} n={stats.del} title="Delete (into the trash)" />
            <Seg cls="s-conf" icon={<Zap size={12} />} n={stats.conflicts} title="Conflict" />
            <Seg icon={<Sigma size={12} />} n={humanSize(stats.bytes) || '0 B'} title="Bytes to transfer" />
            {stats.flips > 0 && <Seg icon={<ArrowLeftRight size={12} />} n={stats.flips} title="Reversed direction" />}
          </>
        )}
      </span>
      {busy && <span className="spinner" />}
    </header>
  );
}
