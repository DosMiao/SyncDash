import { useEffect, useRef } from 'react';
import { activeNow, windowRate } from './runstate';
import type { RunState } from './runstate';
import type { RefObject } from 'react';

interface Props {
  caption: string;
  field: 'b' | 'i';
  /// The live run state, read on each redraw. The canvas repaints on its own 100 ms timer so the curve
  /// stays smooth without dragging the numeric readouts (throttled to 500 ms) along with it.
  runRef: RefObject<RunState>;
  rateText: (s: RunState) => string;
}

function draw(cv: HTMLCanvasElement, s: RunState, key: 'b' | 'i', total: number) {
  const ctx = cv.getContext('2d');
  if (!ctx) return;
  const dpr = window.devicePixelRatio || 1;
  const w = cv.clientWidth, h = cv.clientHeight;
  if (cv.width !== w * dpr || cv.height !== h * dpr) { cv.width = w * dpr; cv.height = h * dpr; }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  // Colors are read from the stylesheet on every frame so the graph tracks the token layer — and,
  // since the tokens flip with prefers-color-scheme, follows a light/dark switch without a reload.
  // The fallbacks are the light-theme values (light is the default) and only ever apply if the
  // stylesheet has not loaded.
  const css = getComputedStyle(document.documentElement);
  const cAccent = css.getPropertyValue('--accent').trim() || '#3b82f6';
  const cDim = css.getPropertyValue('--text-2').trim() || '#5c5c63';
  const cBorder = css.getPropertyValue('--border').trim() || '#e0e0e3';

  const now = activeNow(s);
  const lastV = s.samples.length ? s.samples[s.samples.length - 1][key] : 0;
  // the ETA endpoint takes part in the x axis: the completion estimate sits at the right edge
  // (a simplified FFS wedge: a dashed marker)
  const r = windowRate(s, 60000);
  const rate = key === 'b' ? r?.bps ?? 0 : r?.ips ?? 0;
  const remain = Math.max(0, total - lastV);
  const etaMs = rate > 1e-6 && total > 0 ? remain / rate * 1000 : 0;
  const xMax = Math.max(now + etaMs, now, 1000);
  const yMax = Math.max(total, lastV, 1);
  const X = (t: number) => t / xMax * (w - 8) + 4;
  const Y = (v: number) => h - 6 - v / yMax * (h - 16);

  // grid
  ctx.strokeStyle = cBorder; ctx.lineWidth = 1;
  for (let g = 1; g <= 3; g++) {
    const y = 6 + (h - 12) * g / 4;
    ctx.beginPath(); ctx.moveTo(2, y); ctx.lineTo(w - 2, y); ctx.stroke();
  }
  // dashed line for the total
  if (total > 0) {
    ctx.setLineDash([4, 3]); ctx.strokeStyle = cDim;
    ctx.beginPath(); ctx.moveTo(2, Y(total)); ctx.lineTo(w - 2, Y(total)); ctx.stroke();
    ctx.setLineDash([]);
  }
  // cumulative curve + fill (decimated by a step once there are >300 samples)
  if (s.samples.length > 1) {
    const step = Math.max(1, Math.floor(s.samples.length / 300));
    ctx.beginPath(); ctx.moveTo(X(0), Y(0));
    for (let k = 0; k < s.samples.length; k += step) ctx.lineTo(X(s.samples[k].t), Y(s.samples[k][key]));
    ctx.lineTo(X(s.samples[s.samples.length - 1].t), Y(lastV));
    ctx.strokeStyle = cAccent; ctx.lineWidth = 1.5; ctx.stroke();
    ctx.lineTo(X(s.samples[s.samples.length - 1].t), Y(0)); ctx.closePath();
    // globalAlpha rather than appending '33' to the color string: that assumed --accent is always
    // a 6-digit hex, which a token written as rgb() or color-mix() would silently break
    ctx.fillStyle = cAccent;
    ctx.globalAlpha = 0.2;
    ctx.fill();
    ctx.globalAlpha = 1;
  }
  // "now" vertical line + estimated-completion dashed line
  ctx.strokeStyle = cDim;
  ctx.beginPath(); ctx.moveTo(X(now), 4); ctx.lineTo(X(now), h - 4); ctx.stroke();
  if (etaMs > 0) {
    ctx.setLineDash([3, 3]);
    ctx.beginPath(); ctx.moveTo(X(now + etaMs), 4); ctx.lineTo(X(now + etaMs), h - 4); ctx.stroke();
    ctx.setLineDash([]);
  }
}

export function Graph({ caption, field, runRef, rateText }: Props) {
  const cvRef = useRef<HTMLCanvasElement>(null);
  const rateRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const id = window.setInterval(() => {
      const s = runRef.current;
      const cv = cvRef.current;
      if (!s || !cv || !s.applying) return;
      draw(cv, s, field, field === 'b' ? s.totals.bytes : s.totals.items);
      // Written straight to the node: this ticks at 10 Hz and has no business re-rendering the tree
      if (rateRef.current) rateRef.current.textContent = rateText(s);
    }, 100);
    return () => window.clearInterval(id);
  }, [field, runRef, rateText]);

  return (
    <div className="gwrap">
      <span className="gcap">{caption}</span>
      <span className="grate" ref={rateRef} />
      <canvas ref={cvRef} />
    </div>
  );
}
