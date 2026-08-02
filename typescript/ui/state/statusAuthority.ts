export type StatusAppearance = '' | 'err' | 'ok';

export interface StatusActionOffer {
  readonly id: number;
  readonly label: string;
  readonly execute: () => Promise<void>;
}

export interface StatusNotice {
  readonly id: number;
  readonly message: string;
  readonly appearance: 'err';
  readonly action: StatusActionOffer | null;
}

export interface StatusState {
  message: string;
  appearance: StatusAppearance;
  action: StatusActionOffer | null;
  actionPending: boolean;
  notices: readonly StatusNotice[];
}

interface ActionRequest {
  offer: StatusActionOffer;
  originatingRevision: number;
  noticeId: number | null;
}

export class StatusAuthority {
  private state: StatusState;
  private statusRevision = 0;
  private nextActionId = 0;
  private nextNoticeId = 0;
  private actionInFlight: ActionRequest | null = null;
  private publish: ((state: StatusState) => void) | null = null;

  constructor(initialMessage = '') {
    this.state = {
      message: initialMessage,
      appearance: '',
      action: null,
      actionPending: false,
      notices: [],
    };
  }

  getState(): StatusState {
    return this.state;
  }

  connect(publish: (state: StatusState) => void): () => void {
    this.publish = publish;
    publish(this.state);
    return () => {
      if (this.publish === publish) this.publish = null;
    };
  }

  setMessage(message: string, appearance: StatusAppearance = ''): void {
    this.statusRevision += 1;
    this.setState({
      ...this.state,
      message,
      appearance,
      action: null,
      actionPending: this.actionInFlight !== null,
    });
  }

  offerAction(
    message: string,
    label: string,
    execute: () => Promise<void>,
    appearance: StatusAppearance = 'ok',
  ): void {
    this.statusRevision += 1;
    this.nextActionId += 1;
    this.setState({
      ...this.state,
      message,
      appearance,
      action: { id: this.nextActionId, label, execute },
      actionPending: this.actionInFlight !== null,
    });
  }

  executeAction(actionId?: number): void {
    const notice = actionId === undefined
      ? null
      : this.state.notices.find((candidate) => candidate.action?.id === actionId) ?? null;
    const offer = actionId === undefined ? this.state.action : notice?.action ?? null;
    if (!offer || this.actionInFlight !== null) return;
    const request: ActionRequest = {
      offer,
      originatingRevision: this.statusRevision,
      noticeId: notice?.id ?? null,
    };
    this.actionInFlight = request;
    this.setState({
      ...this.state,
      action: this.state.action?.id === offer.id ? null : this.state.action,
      actionPending: true,
      notices: this.state.notices.map((candidate) => (
        candidate.id === request.noticeId ? { ...candidate, action: null } : candidate
      )),
    });
    void offer.execute().then(
      () => this.completeAction(request),
      (error: unknown) => this.failAction(request, error),
    );
  }

  dismissNotice(noticeId: number): void {
    if (this.actionInFlight?.noticeId === noticeId) return;
    this.setState({
      ...this.state,
      notices: this.state.notices.filter((notice) => notice.id !== noticeId),
    });
  }

  private completeAction(request: ActionRequest): void {
    if (this.actionInFlight !== request) return;
    this.actionInFlight = null;
    this.setState({
      ...this.state,
      actionPending: false,
      notices: request.noticeId === null
        ? this.state.notices
        : this.state.notices.filter((candidate) => candidate.id !== request.noticeId),
    });
  }

  private failAction(request: ActionRequest, error: unknown): void {
    if (this.actionInFlight !== request) return;
    this.actionInFlight = null;
    const failureMessage = `${request.offer.label} failed: ${error}`;
    const actionWasSuperseded = this.statusRevision !== request.originatingRevision;
    this.statusRevision += 1;

    if (request.noticeId !== null) {
      this.setState({
        ...this.state,
        actionPending: false,
        notices: this.state.notices.map((candidate) => (
          candidate.id === request.noticeId
            ? { ...candidate, message: failureMessage, action: request.offer }
            : candidate
        )),
      });
      return;
    }
    if (!actionWasSuperseded) {
      this.setState({
        ...this.state,
        message: failureMessage,
        appearance: 'err',
        action: request.offer,
        actionPending: false,
      });
      return;
    }
    this.nextNoticeId += 1;
    const failureNotice: StatusNotice = {
      id: this.nextNoticeId,
      message: failureMessage,
      appearance: 'err',
      action: request.offer,
    };
    this.setState({
      ...this.state,
      actionPending: false,
      notices: [...this.state.notices, failureNotice],
    });
  }

  private setState(state: StatusState): void {
    this.state = state;
    this.publish?.(state);
  }
}
