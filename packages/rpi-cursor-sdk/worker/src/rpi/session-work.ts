import { AsyncLocalStorage } from "node:async_hooks";
import type { NativeSessionOrigin } from "./session.js";

const current = new AsyncLocalStorage<NativeSessionWork>();

/** A provider result may precede SDK completion; its origins live until both have finished. */
export class NativeSessionWork {
	private releases = new Map<NativeSessionOrigin, () => Promise<void>>();
	private tasks = new Set<Promise<unknown>>();
	private failures: unknown[] = [];
	private started = false;
	private returned = false;
	private closed = false;
	private resolve!: () => void;
	private reject!: (error: unknown) => void;
	readonly settled = new Promise<void>((resolve, reject) => { this.resolve = resolve; this.reject = reject; });
	constructor(origin: NativeSessionOrigin) {
		this.hold(origin);
		// Runtime shutdown also awaits this receipt, including failures after the provider returned.
		void this.settled.catch(() => {});
	}
	private assertOpen(): void { if (this.closed) throw new Error("Native session work is closed"); }
	hold(origin: NativeSessionOrigin): void {
		this.assertOpen();
		if (!this.releases.has(origin)) this.releases.set(origin, origin.retain());
	}
	async run<T>(body: () => Promise<T>): Promise<T> {
		this.assertOpen();
		if (this.started) throw new Error("Native session work already started");
		this.started = true;
		return current.run(this, async () => {
			try { return await body(); }
			finally { this.returned = true; this.finish(); }
		});
	}
	track<T>(task: Promise<T>): Promise<T> {
		this.assertOpen();
		if (this.tasks.has(task)) return task;
		this.tasks.add(task);
		const complete = () => { this.tasks.delete(task); this.finish(); };
		void task.then(complete, error => { this.failures.push(error); complete(); });
		return task;
	}
	callback<A extends unknown[], R>(body: (...args: A) => Promise<R>): (...args: A) => Promise<R> {
		const bound = AsyncLocalStorage.bind(body);
		return async (...args) => { this.assertOpen(); return this.track(bound(...args)); };
	}
	private finish(): void {
		if (!this.returned || this.tasks.size || this.closed) return;
		this.closed = true;
		void Promise.allSettled([...this.releases.values()].map(release => release())).then(results => {
			this.releases.clear();
			for (const result of results) if (result.status === "rejected") this.failures.push(result.reason);
			if (this.failures.length) this.reject(new AggregateError(this.failures, "Native session background work failed"));
			else this.resolve();
		});
	}
}

export function holdNativeSessionOrigin(origin: NativeSessionOrigin | undefined): void {
	if (origin) current.getStore()?.hold(origin);
}
export function trackNativeSessionWork(task: Promise<unknown>): void { current.getStore()?.track(task); }
export function bindNativeSessionCallback<A extends unknown[], R>(body: (...args: A) => Promise<R>): (...args: A) => Promise<R> {
	return current.getStore()?.callback(body) ?? body;
}
