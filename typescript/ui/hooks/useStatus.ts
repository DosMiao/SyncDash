import { useCallback, useRef, useState } from 'react';

export type StatusClass = '' | 'err' | 'ok';

export interface UndoOffer { label: string; run: () => Promise<void> }

export interface StatusState {
  msg: string;
  cls: StatusClass;
  /// Any action that edits the job file on the user's behalf must offer a way back
  undo: UndoOffer | null;
}

export interface StatusApi {
  status: StatusState;
  /// Plain message; always clears a pending undo — the offer belonged to the action that just got replaced
  set: (msg: string, cls?: StatusClass) => void;
  withUndo: (msg: string, label: string, run: () => Promise<void>, cls?: StatusClass) => void;
  runUndo: () => void;
}

export function useStatus(initial = ''): StatusApi {
  const [status, setStatus] = useState<StatusState>({ msg: initial, cls: '', undo: null });
  // The undo body is called from an event handler that must not re-render on every status change
  const ref = useRef(status);
  ref.current = status;

  const set = useCallback((msg: string, cls: StatusClass = '') => {
    setStatus({ msg, cls, undo: null });
  }, []);

  const withUndo = useCallback((msg: string, label: string, run: () => Promise<void>, cls: StatusClass = 'ok') => {
    setStatus({ msg, cls, undo: { label, run } });
  }, []);

  const runUndo = useCallback(() => {
    const offer = ref.current.undo;
    if (!offer) return;
    setStatus((s) => ({ ...s, undo: null }));
    offer.run().catch((e) => setStatus({ msg: `Undo failed: ${e}`, cls: 'err', undo: null }));
  }, []);

  return { status, set, withUndo, runUndo };
}
