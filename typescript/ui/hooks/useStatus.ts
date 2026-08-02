import { useCallback, useEffect, useState } from 'react';
import { StatusAuthority } from '../state/statusAuthority';
import type { StatusAppearance, StatusState } from '../state/statusAuthority';

export type {
  StatusActionOffer,
  StatusAppearance,
  StatusNotice,
  StatusState,
} from '../state/statusAuthority';

export interface StatusApi {
  status: StatusState;
  setMessage: (message: string, appearance?: StatusAppearance) => void;
  offerAction: (
    message: string,
    label: string,
    execute: () => Promise<void>,
    appearance?: StatusAppearance,
  ) => void;
  executeAction: (actionId?: number) => void;
  dismissNotice: (noticeId: number) => void;
}

export function useStatus(initialMessage = ''): StatusApi {
  const [authority] = useState(() => new StatusAuthority(initialMessage));
  const [status, setStatus] = useState(() => authority.getState());
  useEffect(() => authority.connect(setStatus), [authority]);

  const setMessage = useCallback(
    (message: string, appearance: StatusAppearance = '') => authority.setMessage(message, appearance),
    [authority],
  );
  const offerAction = useCallback((
    message: string,
    label: string,
    execute: () => Promise<void>,
    appearance: StatusAppearance = 'ok',
  ) => authority.offerAction(message, label, execute, appearance), [authority]);
  const executeAction = useCallback((actionId?: number) => authority.executeAction(actionId), [authority]);
  const dismissNotice = useCallback((noticeId: number) => authority.dismissNotice(noticeId), [authority]);

  return { status, setMessage, offerAction, executeAction, dismissNotice };
}
