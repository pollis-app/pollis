import { makeAutoObservable } from "mobx";

/** #1735: which history transfers this window is driving right now (UI state only; the transfers live in React Query). */
class HistoryTransferStore {
  running: string[] = [];

  constructor() {
    makeAutoObservable(this, {}, { autoBind: true });
  }

  setRunning(transferId: string, running: boolean) {
    const without = this.running.filter((id) => id !== transferId);
    this.running = running ? [...without, transferId] : without;
  }

  isRunning(transferId: string): boolean {
    return this.running.includes(transferId);
  }

  clear() {
    this.running = [];
  }
}

export const historyTransferStore = new HistoryTransferStore();
