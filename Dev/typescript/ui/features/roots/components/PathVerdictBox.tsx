import { Check, Info, TriangleAlert } from 'lucide-react';
import type { PathInspectionState } from '#ui/features/roots/hooks/usePathVerdict.ts';

/// Warnings from inspect_paths plus the marker confirmation, in the shape both the main path row
/// (.pwarn) and the job editor (.ed-verdict) render.
export function PathVerdictBox({ inspection, className }: { inspection: PathInspectionState; className: string }) {
  if (inspection.status === 'inactive' || inspection.status === 'debouncing') return <div className={className} />;
  if (inspection.status === 'checking') {
    return <div className={className}><div className="vnote" role="status"><Info size={12} /> Checking roots…</div></div>;
  }
  if (inspection.status === 'failed') {
    return <div className={className}><div className="vwarn" role="alert"><TriangleAlert size={12} /> Root inspection failed: {inspection.error}</div></div>;
  }
  const { verdict } = inspection;
  const marks = [
    verdict.source.has_marker ? 'source has a .syncdash-root marker' : '',
    verdict.target.has_marker ? 'target has a .syncdash-root marker' : '',
  ].filter(Boolean).join(' · ');

  return (
    <div className={className}>
      {verdict.warnings.map((w, k) => (
        <div className="vwarn" key={`warning-${k}`}><TriangleAlert size={12} /> {w}</div>
      ))}
      {verdict.notes.map((note, k) => (
        <div className="vnote" key={`note-${k}`}><Info size={12} /> {note}</div>
      ))}
      {marks && <div className="vok"><Check size={12} /> {marks}</div>}
    </div>
  );
}
