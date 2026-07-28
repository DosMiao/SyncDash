import { ArrowLeft, ArrowRight, Copy, CornerUpRight, Info, Pencil, Trash2, Zap } from 'lucide-react';
import type { ReactNode } from 'react';
import type { Dir, Kind } from '../core/plan';

// One glyph per plan category, shared by the three surfaces that speak this vocabulary: a row's
// action cell, the category chips and the toolbar stats. A chip, a stat and a row for the same
// action are then literally the same mark, and a new category is one line here rather than three.
//
// Which glyph draws a category is a rendering decision, so it stays out of core/plan.ts, which is
// React-free by contract. *Which* category a row is stays there, in category(). This file exists
// rather than the map living beside the table because FilterBar and Toolbar would otherwise have to
// import from PlanTable, which is worse than either.

/// All three surfaces draw at 12px, so these are prebuilt nodes rather than components. Every icon
/// inherits `currentColor`, which is what lets one `--k` hue drive the glyph and the label together.
export const MARK: Record<Kind, ReactNode> = {
  copy: <Copy size={12} />,
  update: <Pencil size={12} />,
  // Not MoveRight: the direction arrow sits immediately to its left, and two arrows side by side
  // read as one confused glyph. The corner arrow says "relocated" and is used nowhere else.
  move: <CornerUpRight size={12} />,
  delete: <Trash2 size={12} />,
  conflict: <Zap size={12} />,
  note: <Info size={12} />,
};

/// Direction of travel — orthogonal to the action, so a separate map: every action that moves bytes
/// has one, and the two reports (conflict, note) have none.
export const DIR_ICON: Record<Dir, ReactNode> = {
  right: <ArrowRight size={12} />,
  left: <ArrowLeft size={12} />,
};
