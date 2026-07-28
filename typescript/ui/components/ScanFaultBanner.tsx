import type { PlanHeader } from '../../core/types/generated/PlanHeader';

/// Says out loud that the scan behind this plan could not read part of a root.
///
/// It is derived from the plan header rather than from the progress stream on purpose. A live
/// event can be missed — the listener may not be mounted, the window may have been closed and
/// reopened, the plan may have been re-read from cache — whereas the header travels with the plan
/// and is still true when the user reaches the Synchronize button minutes later. It renders from
/// the complete header or not at all; there is no partial state to show.
///
/// The consequence is the part worth spelling out: compare cannot distinguish an entry that was
/// never read from one that was deleted, so under mirror an unread subtree becomes deletions on the
/// other side. Naming the paths matters more than the count — on macOS the usual cause is a
/// privacy permission, and the path is what tells the user which one.
export function ScanFaultBanner({ header }: { header: PlanHeader }) {
  const total = header.source_walk_errors + header.target_walk_errors;
  if (total === 0) return null;

  const sides: string[] = [];
  if (header.source_walk_errors > 0) sides.push(`${header.source_walk_errors} on the source`);
  if (header.target_walk_errors > 0) sides.push(`${header.target_walk_errors} on the target`);
  const samples = [...header.source_walk_err_samples, ...header.target_walk_err_samples];

  return (
    <div className="scanfault" role="alert">
      <span>
        <b>This comparison is incomplete.</b>{' '}
        The scan could not read {sides.join(' and ')}. Those entries are missing from the table
        below, and a missing entry is indistinguishable from a deleted one — in mirror mode they
        become deletions on the other side.
      </span>
      {samples.length > 0 && (
        <ul>
          {samples.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ul>
      )}
      <span>Fix the cause and compare again, rather than synchronizing this plan.</span>
    </div>
  );
}
