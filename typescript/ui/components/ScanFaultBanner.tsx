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
/// Two distinct faults share one banner because they share one consequence: the table below does
/// not describe the tree. They are kept as separate sentences, not merged into a total, because the
/// remedies are different — a permission versus a download.
function sideList(source: number, target: number): string {
  const parts: string[] = [];
  if (source > 0) parts.push(`${source} on the source`);
  if (target > 0) parts.push(`${target} on the target`);
  return parts.join(' and ');
}

export function ScanFaultBanner({ header }: { header: PlanHeader }) {
  const unread = header.source_walk_errors + header.target_walk_errors;
  const stubs = header.source_icloud_stubs + header.target_icloud_stubs;
  if (unread === 0 && stubs === 0) return null;

  const unreadSamples = [...header.source_walk_err_samples, ...header.target_walk_err_samples];
  const stubSamples = [...header.source_icloud_stub_samples, ...header.target_icloud_stub_samples];

  return (
    <div className="scanfault" role="alert">
      <b>This comparison does not describe the whole tree.</b>
      {unread > 0 && (
        <>
          <span>
            The scan could not read {sideList(header.source_walk_errors, header.target_walk_errors)}.
            Those entries are missing from the table below, and a missing entry is
            indistinguishable from a deleted one — in mirror mode they become deletions on the
            other side.
          </span>
          {unreadSamples.length > 0 && (
            <ul>
              {unreadSamples.map((s) => (
                <li key={s}>{s}</li>
              ))}
            </ul>
          )}
        </>
      )}
      {stubs > 0 && (
        <>
          <span>
            {sideList(header.source_icloud_stubs, header.target_icloud_stubs)} are iCloud
            placeholders — their contents are not on this disk, and their real names are absent from
            the table. Synchronizing would copy the placeholder over the real file on the other side
            and delete the original. Excluding them does not help: the delete comes from the missing
            name, not from the placeholder. Download them first, or exclude the whole tree.
          </span>
          {stubSamples.length > 0 && (
            <ul>
              {stubSamples.map((s) => (
                <li key={s}>{s}</li>
              ))}
            </ul>
          )}
        </>
      )}
      <span>Fix the cause and compare again, rather than synchronizing this plan.</span>
    </div>
  );
}
