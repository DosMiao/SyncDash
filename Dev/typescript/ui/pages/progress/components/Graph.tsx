import { useEffect, useRef } from 'react';
import { activeElapsedMs, calculateWindowRate } from '#core/application/progress/runstate.ts';
import type { RunState } from '#core/application/progress/runstate.ts';
import type { RefObject } from 'react';

type GraphMetric = 'bytesDone' | 'itemsDone';

interface GraphProps {
  caption: string;
  metric: GraphMetric;
  runRef: RefObject<RunState>;
  rateText: (state: RunState) => string;
}

function drawGraph(
  canvas: HTMLCanvasElement,
  state: RunState,
  metric: GraphMetric,
  total: number,
) {
  const context = canvas.getContext('2d');
  if (!context) return;
  const pixelRatio = window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (canvas.width !== width * pixelRatio || canvas.height !== height * pixelRatio) {
    canvas.width = width * pixelRatio;
    canvas.height = height * pixelRatio;
  }
  context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
  context.clearRect(0, 0, width, height);

  const documentStyle = getComputedStyle(document.documentElement);
  const accentColor = documentStyle.getPropertyValue('--accent').trim() || '#3b82f6';
  const mutedColor = documentStyle.getPropertyValue('--text-2').trim() || '#5c5c63';
  const borderColor = documentStyle.getPropertyValue('--border').trim() || '#e0e0e3';

  const activeTimeMs = activeElapsedMs(state);
  const latestValue = state.samples.length ? state.samples[state.samples.length - 1][metric] : 0;
  const rate = calculateWindowRate(state, 60_000);
  const unitsPerSecond = metric === 'bytesDone'
    ? rate?.bytesPerSecond ?? 0
    : rate?.itemsPerSecond ?? 0;
  const remaining = Math.max(0, total - latestValue);
  const estimatedRemainingMs = unitsPerSecond > 1e-6 && total > 0
    ? remaining / unitsPerSecond * 1000
    : 0;
  const maximumTimeMs = Math.max(activeTimeMs + estimatedRemainingMs, activeTimeMs, 1000);
  const maximumValue = Math.max(total, latestValue, 1);
  const xCoordinate = (timeMs: number) => timeMs / maximumTimeMs * (width - 8) + 4;
  const yCoordinate = (value: number) => height - 6 - value / maximumValue * (height - 16);

  context.strokeStyle = borderColor;
  context.lineWidth = 1;
  for (let gridLine = 1; gridLine <= 3; gridLine++) {
    const y = 6 + (height - 12) * gridLine / 4;
    context.beginPath();
    context.moveTo(2, y);
    context.lineTo(width - 2, y);
    context.stroke();
  }

  if (total > 0) {
    context.setLineDash([4, 3]);
    context.strokeStyle = mutedColor;
    context.beginPath();
    context.moveTo(2, yCoordinate(total));
    context.lineTo(width - 2, yCoordinate(total));
    context.stroke();
    context.setLineDash([]);
  }

  if (state.samples.length > 1) {
    const sampleStep = Math.max(1, Math.floor(state.samples.length / 300));
    context.beginPath();
    context.moveTo(xCoordinate(0), yCoordinate(0));
    for (let index = 0; index < state.samples.length; index += sampleStep) {
      const sample = state.samples[index];
      context.lineTo(xCoordinate(sample.activeElapsedMs), yCoordinate(sample[metric]));
    }
    const latestSample = state.samples[state.samples.length - 1];
    context.lineTo(xCoordinate(latestSample.activeElapsedMs), yCoordinate(latestValue));
    context.strokeStyle = accentColor;
    context.lineWidth = 1.5;
    context.stroke();
    context.lineTo(xCoordinate(latestSample.activeElapsedMs), yCoordinate(0));
    context.closePath();
    context.fillStyle = accentColor;
    context.globalAlpha = 0.2;
    context.fill();
    context.globalAlpha = 1;
  }

  context.strokeStyle = mutedColor;
  context.beginPath();
  context.moveTo(xCoordinate(activeTimeMs), 4);
  context.lineTo(xCoordinate(activeTimeMs), height - 4);
  context.stroke();
  if (estimatedRemainingMs > 0) {
    context.setLineDash([3, 3]);
    context.beginPath();
    context.moveTo(xCoordinate(activeTimeMs + estimatedRemainingMs), 4);
    context.lineTo(xCoordinate(activeTimeMs + estimatedRemainingMs), height - 4);
    context.stroke();
    context.setLineDash([]);
  }
}

export function Graph({ caption, metric, runRef, rateText }: GraphProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rateRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const intervalId = window.setInterval(() => {
      const state = runRef.current;
      const canvas = canvasRef.current;
      if (!state || !canvas || !state.applying) return;
      drawGraph(
        canvas,
        state,
        metric,
        metric === 'bytesDone' ? state.totals.bytes : state.totals.items,
      );
      if (rateRef.current) rateRef.current.textContent = rateText(state);
    }, 100);
    return () => window.clearInterval(intervalId);
  }, [metric, runRef, rateText]);

  return (
    <div className="gwrap">
      <span className="gcap">{caption}</span>
      <span className="grate" ref={rateRef} />
      <canvas ref={canvasRef} aria-hidden="true" />
    </div>
  );
}
