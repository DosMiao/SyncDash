import type { ItemOutcome } from '#core/types/generated/ItemOutcome.ts';
import type { LogLevel } from '#core/types/generated/LogLevel.ts';
import type { Phase } from '#core/types/generated/Phase.ts';
import type { PhaseStatus } from '#core/types/generated/PhaseStatus.ts';
import type { RunEventDto } from '#core/types/generated/RunEventDto.ts';

/**
 * The wire envelope, generated from Rust rather than mirrored by hand.
 *
 * It is an intersection with `ProgressEvent`, so `kind` still discriminates and a consumer that
 * reads `phase` has to establish which variant it holds. The previous hand-written mirror
 * flattened every variant into one bag of optional fields, which made every such read a guess.
 */
export type RunEventEnvelope = RunEventDto;

/** The terminal event of a run. Every apply emits exactly one. */
export type RunSummaryEvent = Extract<RunEventDto, { kind: 'summary' }>;

/** The variants that carry item/byte counters. */
export type RunCountedEvent = Extract<RunEventDto, { items_total: number }>;

export function isSummaryEvent(event: RunEventEnvelope): event is RunSummaryEvent {
  return event.kind === 'summary';
}

export function isCountedEvent(event: RunEventEnvelope): event is RunCountedEvent {
  return event.kind === 'phase_start' || event.kind === 'totals'
    || event.kind === 'progress' || event.kind === 'phase_end';
}

/** The phase an event belongs to, or undefined for the events that name no phase. */
export function eventPhase(event: RunEventEnvelope): Phase | undefined {
  return 'phase' in event ? event.phase : undefined;
}

/** The human label an event carries, if its variant has one. */
export function eventLabel(event: RunEventEnvelope): string | null | undefined {
  return 'label' in event ? event.label : undefined;
}
export function mergeRunEventReplay(
  replay: RunEventEnvelope[],
  queued: RunEventEnvelope[],
): RunEventEnvelope[] {
  const ordered = [...replay, ...queued]
    .sort((left, right) => left.sequence - right.sequence);
  return ordered.filter((event, index) => (
    index === 0 || event.sequence !== ordered[index - 1].sequence
  ));
}
