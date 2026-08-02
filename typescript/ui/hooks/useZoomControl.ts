import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ZoomAuthority,
  applyZoom,
  createInitialZoomAuthorityState,
  loadZoomPreference,
  saveZoomPreference,
  stepZoom,
} from '../../core/zoom';
import type { ZoomAuthorityFailure, ZoomFactor } from '../../core/zoom';

function describeZoomFailure(failure: ZoomAuthorityFailure): string {
  const percentage = Math.round(failure.factor * 100);
  if (failure.kind === 'persistence') {
    return `Zoom is ${percentage}% for this session only; the preference could not be saved: ${failure.error}`;
  }
  const action = failure.reason === 'restore' ? 'restore' : 'change';
  return `Could not ${action} the interface zoom to ${percentage}%: ${String(failure.error)}`;
}

export function useZoomControl(onError: (message: string) => void) {
  const [initialPreference] = useState(() => loadZoomPreference());
  const [initialAuthorityState] = useState(() => (
    createInitialZoomAuthorityState(initialPreference)
  ));
  const [authorityState, setAuthorityState] = useState(initialAuthorityState);
  const desiredZoomFactorRef = useRef<ZoomFactor>(initialAuthorityState.desiredFactor);
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const preferenceWarningReportedRef = useRef(false);
  const zoomAuthorityRef = useRef<ZoomAuthority | null>(null);

  if (zoomAuthorityRef.current === null) {
    zoomAuthorityRef.current = new ZoomAuthority(initialAuthorityState, {
      applyWebviewZoom: applyZoom,
      persistPreference: (factor) => saveZoomPreference(factor),
      publishState: (state) => {
        desiredZoomFactorRef.current = state.desiredFactor;
        setAuthorityState(state);
      },
      onFailure: (failure) => onErrorRef.current(describeZoomFailure(failure)),
    });
  }
  const zoomAuthority = zoomAuthorityRef.current;

  useEffect(() => {
    if (initialPreference.warning && !preferenceWarningReportedRef.current) {
      preferenceWarningReportedRef.current = true;
      onErrorRef.current(initialPreference.warning);
    }
    desiredZoomFactorRef.current = initialPreference.factor;
    zoomAuthority.requestZoom(initialPreference.factor, {
      persist: false,
      reason: 'restore',
    });
    return () => zoomAuthority.cancelPendingRequests();
  }, [initialPreference, zoomAuthority]);

  const requestZoomChange = useCallback((nextFactor: ZoomFactor) => {
    desiredZoomFactorRef.current = nextFactor;
    zoomAuthority.requestZoom(nextFactor, {
      persist: true,
      reason: 'change',
    });
  }, [zoomAuthority]);

  const stepDesiredZoom = useCallback((direction: 1 | -1) => {
    requestZoomChange(stepZoom(desiredZoomFactorRef.current, direction));
  }, [requestZoomChange]);

  const zoomIn = useCallback(() => stepDesiredZoom(1), [stepDesiredZoom]);
  const zoomOut = useCallback(() => stepDesiredZoom(-1), [stepDesiredZoom]);
  const zoomReset = useCallback(() => requestZoomChange(1), [requestZoomChange]);

  return {
    zoom: authorityState.appliedFactor,
    zoomIn,
    zoomOut,
    zoomReset,
  };
}
