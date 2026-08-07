import { TriangleAlert } from 'lucide-react';
import type { PlanHeader } from '#core/types/generated/PlanHeader.ts';

/// Says out loud that the scan behind this plan could not read part of a root — from the toolbar,
/// on demand.
///
/// It is derived from the plan header rather than from the progress stream on purpose. A live
/// event can be missed — the listener may not be mounted, the window may have been closed and
/// reopened, the plan may have been re-read from cache — whereas the header travels with the plan
/// and is still true when the user reaches the Synchronize button minutes later. It renders from
/// the complete header or not at all; there is no partial state to show.
///
/// A mark in the toolbar rather than a block above the table. The results viewport is what the
/// window is for, and the fault text is as long as the number of denied directories, so a banner
/// there costs the most valuable rows on screen and grows without bound on exactly the tree that
/// needed reading. The mark is always visible and the detail is one click away.
///
/// The severity split is the part worth getting right. An *unread subtree* took no part in the
/// comparison at all — compare suppressed both sides there — so this plan is safe to run; it is
/// smaller than the user asked for, not wrong. A *walk error* or an *iCloud placeholder* is a
/// genuinely absent entry that mirror will turn into a deletion. Only the second kind earns the
/// red mark and the "do not synchronize this plan" line; spending them on the first would train
/// the user to ignore both.
function sideList(source: number, target: number): string {
  const parts: string[] = [];
  if (source > 0) parts.push(`${source} on the source`);
  if (target > 0) parts.push(`${target} on the target`);
  return parts.join(' and ');
}

export function ScanFaultIndicator({ header }: { header: PlanHeader | null }) {
  if (!header) return null;
  const missing = header.source_walk_errors + header.target_walk_errors;
  const stubs = header.source_icloud_stubs + header.target_icloud_stubs;
  const unreadPaths = [...header.source_unread_paths, ...header.target_unread_paths];
  if (missing === 0 && stubs === 0 && unreadPaths.length === 0) return null;

  const missingSamples = [...header.source_walk_err_samples, ...header.target_walk_err_samples];
  const stubSamples = [...header.source_icloud_stub_samples, ...header.target_icloud_stub_samples];
  const hidden = header.source_unread_entries + header.target_unread_entries;
  const unsafe = missing > 0 || stubs > 0;
  const faults = (unreadPaths.length > 0 ? 1 : 0) + (missing > 0 ? 1 : 0) + (stubs > 0 ? 1 : 0);
  const summary = unsafe
    ? 'This comparison does not describe the whole tree, and this plan would act on the difference'
    : 'This comparison does not describe the whole tree';

  return (
    /// Native disclosure rather than component state: the open flag has exactly one owner, the
    /// keyboard and screen-reader behavior comes for free, and nothing here has to survive a
    /// re-render of the toolbar around it.
    <details className={'scanfault' + (unsafe ? ' unsafe' : '')}>
      <summary
        className="scanfault-mark"
        title={`${summary}. Click for detail.`}
        aria-label={`${summary}. ${faults} scan fault(s). Click for detail.`}
      >
        <TriangleAlert size={13} />
        <b>{faults}</b>
      </summary>
      <div className="scanfault-pop" role={unsafe ? 'alert' : 'status'}>
        <b className="scanfault-title">{summary}.</b>
        {unreadPaths.length > 0 && (
          <section>
            <b>{unreadPaths.length} subtree(s) could not be read.</b>
            <span>
              They were left out of this comparison on both sides — nothing under them is copied,
              deleted, or counted as a difference
              {hidden > 0 ? `, and ${hidden} known entr(ies) took no part in the result` : ''}.
              Grant access to these paths and compare again to include them.
            </span>
            <ul>
              {unreadPaths.map((path) => (
                <li key={path}>{path}</li>
              ))}
            </ul>
          </section>
        )}
        {missing > 0 && (
          <section>
            <b>
              The scan could not read{' '}
              {sideList(header.source_walk_errors, header.target_walk_errors)}.
            </b>
            <span>
              Those entries are missing from the table, and a missing entry is indistinguishable
              from a deleted one — in mirror mode they become deletions on the other side.
            </span>
            {missingSamples.length > 0 && (
              <ul>
                {missingSamples.map((sample) => (
                  <li key={sample}>{sample}</li>
                ))}
              </ul>
            )}
          </section>
        )}
        {stubs > 0 && (
          <section>
            <b>
              {sideList(header.source_icloud_stubs, header.target_icloud_stubs)} are iCloud
              placeholders.
            </b>
            <span>
              Their contents are not on this disk, and their real names are absent from the table.
              Synchronizing would copy the placeholder over the real file on the other side and
              delete the original. Excluding them does not help: the delete comes from the missing
              name, not from the placeholder. Download them first, or exclude the whole tree.
            </span>
            {stubSamples.length > 0 && (
              <ul>
                {stubSamples.map((sample) => (
                  <li key={sample}>{sample}</li>
                ))}
              </ul>
            )}
          </section>
        )}
        {unsafe && (
          <b className="scanfault-act">
            Fix the cause and compare again, rather than synchronizing this plan.
          </b>
        )}
      </div>
    </details>
  );
}
