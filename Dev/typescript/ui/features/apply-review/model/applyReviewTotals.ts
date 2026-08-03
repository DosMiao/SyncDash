import { effectiveOperation, rowTransferBytes, type PlanDto } from '#core/domain/compare/plan.ts';

export interface ApplyReviewTotals {
  copyCount: number;
  updateCount: number;
  moveCount: number;
  deleteCount: number;
  transferBytes: number;
  deletionBytes: number;
  reversedCount: number;
  checkedOutsideScope: number;
  /** The target's own entry count, which the overwrite and move shares are measured against. */
  targetEntries: number;
  /** Both sides, in the order the engine checks them. Deletions are judged per side. */
  deletionSides: DeletionSideTotals[];
}

/**
 * The engine's own deletion-share inputs for one side, as `guard/ratio.rs` receives them from
 * `guard/mod.rs`: the `delete` rows executing on that side, against that side's own snapshot entry
 * count. `delete_dir` is not in the numerator because the engine keeps directory removals in a
 * separate field (`guard/stats.rs`) and never feeds them to the ratio.
 */
export interface DeletionSideTotals {
  side: 'source' | 'target';
  deletes: number;
  entries: number;
}

export function summarizeApplyReview(
  plan: PlanDto,
  executableIndices: number[],
  reversedRows: boolean[],
  includedRows: boolean[],
): ApplyReviewTotals {
  const deletesBySide: Record<'source' | 'target', number> = { source: 0, target: 0 };
  const totals: ApplyReviewTotals = {
    copyCount: 0,
    updateCount: 0,
    moveCount: 0,
    deleteCount: 0,
    transferBytes: 0,
    deletionBytes: 0,
    reversedCount: 0,
    checkedOutsideScope: 0,
    targetEntries: plan.header.target_entries,
    deletionSides: [],
  };
  for (const index of executableIndices) {
    const operation = effectiveOperation(plan, reversedRows, index);
    if (operation.action === 'copy') {
      totals.copyCount++;
      totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
    } else if (operation.action === 'update') {
      totals.updateCount++;
      totals.transferBytes += rowTransferBytes(plan, reversedRows, index);
    } else if (operation.action === 'chmod') {
      totals.updateCount++;
    } else if (operation.action === 'move') {
      totals.moveCount++;
    } else if (operation.action === 'delete' || operation.action === 'delete_dir') {
      totals.deleteCount++;
      totals.deletionBytes += operation.size ?? 0;
      // The share counts what the engine counts, and the engine keeps directory removals out of it.
      // Reversal has already moved the row to the other side, exactly as `reverse_op` does before
      // the engine splits its statistics by side.
      if (operation.action === 'delete') deletesBySide[operation.side]++;
    }
    if (reversedRows[index]) totals.reversedCount++;
  }
  totals.deletionSides = [
    { side: 'target', deletes: deletesBySide.target, entries: plan.header.target_entries },
    { side: 'source', deletes: deletesBySide.source, entries: plan.header.source_entries },
  ];
  totals.checkedOutsideScope = includedRows.filter(Boolean).length - executableIndices.length;
  return totals;
}

/**
 * How much of what a side already holds a category of the run displaces.
 *
 * Measured against that side's entry count rather than the plan's own size: "1000 of 1000 rows are
 * deletes" says nothing on its own, while "1000 of 1200 entries" is the number that distinguishes a
 * deliberate cleanup from a wrong filter, a swapped source and target, or an unmounted share. The
 * count and the entries have to describe the same side, or the result is not a share of anything:
 * source-side deletes measured against the target's entries printed "900% of target deleted" for a
 * plan that deleted nothing on the target.
 *
 * Only pass a count of operations that displace existing data. A plain copy publishes onto a name
 * the target does not have, so it is not part of any share — counting copies here reported
 * "792350% of target" for a first mirror onto a near-empty disk, which is arithmetic rather than
 * information. Returns null when there is nothing to measure against, so a caller shows no share at
 * all rather than a made-up zero.
 */
export function planShare(count: number, entries: number): number | null {
  if (count <= 0 || entries <= 0) return null;
  return count / entries;
}

/**
 * Whether a category is large enough that the review panel should color it.
 *
 * A threshold outside (0, 1) is the job switching the highlight off, matching how the engine reads
 * the same number. This marks a row for attention; it never withholds the run.
 */
export function planShareIsHigh(
  count: number,
  entries: number,
  threshold: number,
): boolean {
  if (!(threshold > 0 && threshold < 1)) return false;
  const share = planShare(count, entries);
  return share !== null && share >= threshold;
}

export function formatPlanShare(count: number, targetEntries: number): string {
  const share = planShare(count, targetEntries);
  return share === null ? '' : `${Math.round(share * 100)}% of target`;
}

/**
 * The deletion side the panel marks, or null when neither is over the mark.
 *
 * The engine judges the two sides independently and can flag both; the sheet has one Delete row, so
 * it names the larger flagged share rather than inventing a combined one. Which side is being
 * emptied is part of the statement: a sync plan can delete most of the source while the target
 * loses nothing.
 */
export function flaggedDeletionSide(
  totals: ApplyReviewTotals,
  threshold: number,
): DeletionSideTotals | null {
  const flagged = totals.deletionSides
    .filter((side) => planShareIsHigh(side.deletes, side.entries, threshold))
    .sort((a, b) => (b.deletes / b.entries) - (a.deletes / a.entries));
  return flagged[0] ?? null;
}

export function formatDeletionShare(side: DeletionSideTotals | null): string {
  if (!side) return '';
  const share = planShare(side.deletes, side.entries);
  return share === null ? '' : `${Math.round(share * 100)}% of ${side.side}`;
}
