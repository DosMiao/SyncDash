import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';

const ZOOM_PREFERENCE_KEY = 'sd.zoom';

/// Discrete webview factors keep text and fixed-pixel layout geometry on the same scale.
export const ZOOM_STEPS = [0.8, 0.9, 1, 1.1, 1.25, 1.4, 1.6, 1.8, 2] as const;

export type ZoomFactor = typeof ZOOM_STEPS[number];

interface StorageReader {
  getItem(key: string): string | null;
}

interface StorageWriter {
  setItem(key: string, value: string): void;
}

export interface ZoomPreferenceLoad {
  factor: ZoomFactor;
  persistedFactor: ZoomFactor | null;
  warning: string | null;
}

export interface ZoomAuthorityState {
  desiredFactor: ZoomFactor;
  appliedFactor: ZoomFactor;
  persistedFactor: ZoomFactor | null;
  latestRequestId: number;
  pending: boolean;
}

export type ZoomRequestReason = 'restore' | 'change';

export type ZoomRequestOptions =
  | { persist: false; reason: 'restore' }
  | { persist: true; reason: 'change' };

export type ZoomAuthorityFailure =
  | {
      kind: 'application';
      factor: ZoomFactor;
      reason: ZoomRequestReason;
      error: unknown;
    }
  | {
      kind: 'persistence';
      factor: ZoomFactor;
      reason: ZoomRequestReason;
      error: string;
    };

interface ZoomAuthorityDependencies {
  applyWebviewZoom(factor: ZoomFactor): Promise<unknown>;
  persistPreference(factor: ZoomFactor): string | null;
  publishState(state: ZoomAuthorityState): void;
  onFailure(failure: ZoomAuthorityFailure): void;
}

interface PendingZoomRequest {
  requestId: number;
  factor: ZoomFactor;
  options: ZoomRequestOptions;
}

export function isZoomFactor(factor: number): factor is ZoomFactor {
  return ZOOM_STEPS.some((candidateFactor) => candidateFactor === factor);
}

function requireZoomFactor(factor: number): ZoomFactor {
  if (!isZoomFactor(factor)) throw new Error(`Invalid interface zoom factor: ${factor}`);
  return factor;
}

export function loadZoomPreference(
  storage: StorageReader = localStorage,
): ZoomPreferenceLoad {
  let storedFactor: string | null;
  try {
    storedFactor = storage.getItem(ZOOM_PREFERENCE_KEY);
  } catch (error) {
    return {
      factor: 1,
      persistedFactor: null,
      warning: `Could not read the interface zoom preference: ${String(error)}`,
    };
  }
  if (storedFactor === null) {
    return { factor: 1, persistedFactor: null, warning: null };
  }
  const parsedFactor = Number(storedFactor);
  if (isZoomFactor(parsedFactor)) {
    return { factor: parsedFactor, persistedFactor: parsedFactor, warning: null };
  }
  return {
    factor: 1,
    persistedFactor: null,
    warning: `Ignored invalid interface zoom preference: ${storedFactor}`,
  };
}

export function saveZoomPreference(
  factor: number,
  storage: StorageWriter = localStorage,
): string | null {
  const validatedFactor = requireZoomFactor(factor);
  try {
    storage.setItem(ZOOM_PREFERENCE_KEY, String(validatedFactor));
    return null;
  } catch (error) {
    return String(error);
  }
}

export async function applyZoom(factor: number): Promise<ZoomFactor> {
  const validatedFactor = requireZoomFactor(factor);
  if (isTauri()) await getCurrentWebview().setZoom(validatedFactor);
  return validatedFactor;
}

export function stepZoom(currentFactor: number, direction: 1 | -1): ZoomFactor {
  const validatedCurrentFactor = requireZoomFactor(currentFactor);
  const currentIndex = ZOOM_STEPS.indexOf(validatedCurrentFactor);
  const nextIndex = Math.min(
    ZOOM_STEPS.length - 1,
    Math.max(0, currentIndex + direction),
  );
  return ZOOM_STEPS[nextIndex];
}

export function createInitialZoomAuthorityState(
  preference: ZoomPreferenceLoad,
): ZoomAuthorityState {
  return {
    desiredFactor: preference.factor,
    appliedFactor: 1,
    persistedFactor: preference.persistedFactor,
    latestRequestId: 0,
    pending: false,
  };
}

export class ZoomAuthority {
  private desiredFactor: ZoomFactor;
  private appliedFactor: ZoomFactor;
  private persistedFactor: ZoomFactor | null;
  private latestRequestId: number;
  private pendingRequest: PendingZoomRequest | null = null;
  private processingRequests = false;
  private readonly dependencies: ZoomAuthorityDependencies;

  constructor(
    initialState: ZoomAuthorityState,
    dependencies: ZoomAuthorityDependencies,
  ) {
    if (initialState.pending) {
      throw new Error('Zoom authority cannot be initialized with an ownerless pending request');
    }
    if (!Number.isSafeInteger(initialState.latestRequestId) || initialState.latestRequestId < 0) {
      throw new Error(`Invalid initial zoom request ID: ${initialState.latestRequestId}`);
    }
    this.desiredFactor = requireZoomFactor(initialState.desiredFactor);
    this.appliedFactor = requireZoomFactor(initialState.appliedFactor);
    this.persistedFactor = initialState.persistedFactor === null
      ? null
      : requireZoomFactor(initialState.persistedFactor);
    this.latestRequestId = initialState.latestRequestId;
    this.dependencies = dependencies;
  }

  getState(): ZoomAuthorityState {
    return {
      desiredFactor: this.desiredFactor,
      appliedFactor: this.appliedFactor,
      persistedFactor: this.persistedFactor,
      latestRequestId: this.latestRequestId,
      pending: this.pendingRequest !== null,
    };
  }

  requestZoom(factor: number, options: ZoomRequestOptions): number {
    const validatedFactor = requireZoomFactor(factor);
    const requestId = this.advanceRequestFence();
    this.desiredFactor = validatedFactor;
    this.pendingRequest = { requestId, factor: validatedFactor, options };
    this.publishState();
    void this.processRequests();
    return requestId;
  }

  cancelPendingRequests(): void {
    this.advanceRequestFence();
    this.pendingRequest = null;
    this.desiredFactor = this.appliedFactor;
  }

  private advanceRequestFence(): number {
    this.latestRequestId += 1;
    return this.latestRequestId;
  }

  private publishState(): void {
    this.dependencies.publishState(this.getState());
  }

  private async processRequests(): Promise<void> {
    if (this.processingRequests) return;
    this.processingRequests = true;
    try {
      while (this.pendingRequest !== null) {
        const request = this.pendingRequest;
        try {
          await this.dependencies.applyWebviewZoom(request.factor);
        } catch (error) {
          if (this.pendingRequest?.requestId !== request.requestId) continue;
          this.pendingRequest = null;
          this.desiredFactor = this.appliedFactor;
          this.publishState();
          this.dependencies.onFailure({
            kind: 'application',
            factor: request.factor,
            reason: request.options.reason,
            error,
          });
          continue;
        }

        // A request can become stale while awaiting. Track the webview's real factor, but publish
        // only the latest request so an intermediate completion never moves the displayed value.
        this.appliedFactor = request.factor;
        if (this.pendingRequest?.requestId !== request.requestId) continue;
        let persistenceError: string | null = null;
        if (request.options.persist) {
          try {
            persistenceError = this.dependencies.persistPreference(request.factor);
          } catch (error) {
            persistenceError = String(error);
          }
          if (persistenceError === null) this.persistedFactor = request.factor;
        }
        if (this.pendingRequest?.requestId !== request.requestId) continue;
        this.pendingRequest = null;
        this.publishState();
        if (persistenceError !== null) {
          this.dependencies.onFailure({
            kind: 'persistence',
            factor: request.factor,
            reason: request.options.reason,
            error: persistenceError,
          });
        }
      }
    } finally {
      this.processingRequests = false;
      if (this.pendingRequest !== null) void this.processRequests();
    }
  }
}
