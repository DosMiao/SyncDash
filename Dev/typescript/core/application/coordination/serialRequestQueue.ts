export class SerialRequestQueue {
  private tail: Promise<void> = Promise.resolve();

  async run<Result>(request: () => Promise<Result>): Promise<Result> {
    const predecessor = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((resolve) => { release = resolve; });
    await predecessor;
    try {
      return await request();
    } finally {
      release();
    }
  }
}
