import { ArrowLeft, ArrowRight, Copy, CornerUpRight, Info, Pencil, Trash2, Zap } from 'lucide-react';
import type { ReactNode } from 'react';
import type { ResultType, ActionDirection } from '../core/plan';

// Rendering stays in the React layer while the exhaustive result-type vocabulary remains in core.
export const RESULT_TYPE_ICON: Record<ResultType, ReactNode> = {
  copy: <Copy size={12} />,
  update: <Pencil size={12} />,
  // Not MoveRight: the direction arrow sits immediately to its left, and two arrows side by side
  // read as one confused glyph. The corner arrow says "relocated" and is used nowhere else.
  move: <CornerUpRight size={12} />,
  delete: <Trash2 size={12} />,
  conflict: <Zap size={12} />,
  note: <Info size={12} />,
};

export const DIRECTION_ICON: Record<ActionDirection, ReactNode> = {
  right: <ArrowRight size={12} />,
  left: <ArrowLeft size={12} />,
};
