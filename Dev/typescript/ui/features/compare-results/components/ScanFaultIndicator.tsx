import { TriangleAlert } from 'lucide-react';
import type { CompareRunFaults } from '#core/domain/compare/compareProgress.ts';
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
/// Two sources, deliberately not one. The plan header is what a finished comparison attests to and
/// survives every re-render; `runFaults` is the live error stream, which is the only place a
/// per-file failure ever appears — a file that changed while it was being read names a path no
/// header carries — and it arrives mid-run, long before there is a plan. The run's `walk` errors
/// are dropped at the reducer instead of here, because the header states the same thing better;
/// see `recordCompareFault`.
///
/// The count is problems, not categories: two denied subtrees read as 2. Counting the sections
/// would have shown 1, which is the sort of number that quietly teaches a reader the badge is
/// decorative.
///
/// The severity split is the part worth getting right, and it is not "how alarming does this
/// sound" but "would running this plan lose data". An *unread subtree* took no part in the
/// comparison, and a *file that could not be read* becomes a reported Conflict that is never
/// auto-arbitrated — both leave the plan safe to run and merely narrower than the user asked for.
/// A *walk error* or an *iCloud placeholder* is a genuinely absent entry that mirror turns into a
/// deletion of the other side's real file. Only that last kind earns the red mark and the "do not
/// synchronize this plan" line; spending them on the others trains the user to ignore all three.
function sideList(source: number, target: number): string {
  const parts: string[] = [];
  if (source > 0) parts.push(`${source} on the source`);
  if (target > 0) parts.push(`${target} on the target`);
  return parts.join(' and ');
}

export function ScanFaultIndicator(
  { header, runFaults }: { header: PlanHeader | null; runFaults: CompareRunFaults },
) {
  const sourceMissing = header?.source_walk_errors ?? 0;
  const targetMissing = header?.target_walk_errors ?? 0;
  const sourceStubs = header?.source_icloud_stubs ?? 0;
  const targetStubs = header?.target_icloud_stubs ?? 0;
  const missing = sourceMissing + targetMissing;
  const stubs = sourceStubs + targetStubs;
  const unreadPaths = header
    ? [...header.source_unread_paths, ...header.target_unread_paths]
    : [];
  const count = unreadPaths.length + missing + stubs + runFaults.total;
  if (count === 0) return null;

  const missingSamples = header
    ? [...header.source_walk_err_samples, ...header.target_walk_err_samples]
    : [];
  const stubSamples = header
    ? [...header.source_icloud_stub_samples, ...header.target_icloud_stub_samples]
    : [];
  const hidden = header ? header.source_unread_entries + header.target_unread_entries : 0;
  // A per-file read failure is deliberately not in here: compare turns it into a reported
  // Conflict, never auto-arbitrated, so the plan is still safe to run. Only the faults that put
  // the other side's data at risk escalate.
  const unsafe = missing > 0 || stubs > 0;
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
        aria-label={`${summary}. ${count} problem(s). Click for detail.`}
      >
        <TriangleAlert size={13} />
        <b>{count}</b>
      </summary>
      <div className="scanfault-pop" role={unsafe ? 'alert' : 'status'}>
        <b className="scanfault-title">{summary}.</b>
        {runFaults.total > 0 && (
          <section>
            <b>
              {runFaults.total} file(s) could not be read during this run.
            </b>
            <span>
              Their content evidence is missing, so they are reported as differences that cannot be
              judged rather than declared identical. A file that changed while it was being read
              usually means something was writing to it — re-run the comparison once it is idle.
              {runFaults.retained.length < runFaults.total
                ? ` Showing the first ${runFaults.retained.length}.`
                : ''}
            </span>
            <ul>
              {runFaults.retained.map((fault) => (
                <li key={`${fault.side}:${fault.path}:${fault.message}`}>
                  {fault.path || '(no path reported)'} — {fault.message}
                </li>
              ))}
            </ul>
          </section>
        )}
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
            <b>The scan could not read {sideList(sourceMissing, targetMissing)}.</b>
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
            <b>{sideList(sourceStubs, targetStubs)} are iCloud placeholders.</b>
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
