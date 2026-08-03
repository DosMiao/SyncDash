export interface RequestTicket {
  readonly owner: string;
  readonly generation: number;
}

export class RequestFence {
  private generation = 0;
  private owner = '';

  start(owner: string): RequestTicket {
    this.owner = owner;
    this.generation += 1;
    return { owner, generation: this.generation };
  }

  invalidate(): void {
    this.owner = '';
    this.generation += 1;
  }

  owns(ticket: RequestTicket): boolean {
    return ticket.owner === this.owner && ticket.generation === this.generation;
  }
}
