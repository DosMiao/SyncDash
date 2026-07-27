import type { PathVerdict } from '../../core/types/generated/PathVerdict';

/// Warnings from inspect_paths plus the marker confirmation, in the shape both the main path row
/// (.pwarn) and the job editor (.ed-verdict) render.
export function PathVerdictBox({ verdict, className }: { verdict: PathVerdict | null; className: string }) {
  if (!verdict) return <div className={className} />;
  const marks = [
    verdict.source.has_marker ? 'source has a .syncdash-root marker' : '',
    verdict.target.has_marker ? 'target has a .syncdash-root marker' : '',
  ].filter(Boolean).join(' · ');

  return (
    <div className={className}>
      {verdict.warnings.map((w, k) => <div className="vwarn" key={k}>⚠ {w}</div>)}
      {marks && <div className="vok">✓ {marks}</div>}
    </div>
  );
}
