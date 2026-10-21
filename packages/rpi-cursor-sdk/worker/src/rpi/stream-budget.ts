import { EVENT_CAPACITY, PAYLOAD_BYTES } from "./protocol.js";

/** One adapter producer, one transport consumer; a cancelled gate never reopens. */
export class StreamBudget {
	readonly shared: SharedArrayBuffer;
	private state: Int32Array;
	constructor(shared = new SharedArrayBuffer(4 * Int32Array.BYTES_PER_ELEMENT)) {
		if (shared.byteLength !== 16) throw new Error("Invalid worker stream budget");
		this.shared = shared;
		this.state = new Int32Array(shared);
	}
	acquire(bytes: number): number {
		const weight = Math.min(PAYLOAD_BYTES, Math.max(1, bytes));
		for (;;) {
			const epoch = Atomics.load(this.state, 2);
			if (Atomics.load(this.state, 3)) throw new DOMException("Worker request cancelled", "AbortError");
			if (Atomics.load(this.state, 0) < EVENT_CAPACITY && weight <= PAYLOAD_BYTES - Atomics.load(this.state, 1)) {
				Atomics.add(this.state, 0, 1);
				Atomics.add(this.state, 1, weight);
				return weight;
			}
			// Only the adapter thread waits. The transport thread continues reading.
			Atomics.wait(this.state, 2, epoch);
		}
	}
	release(weight: number): void {
		if (weight < 1 || weight > PAYLOAD_BYTES || Atomics.load(this.state, 0) < 1 || Atomics.load(this.state, 1) < weight) throw new Error("Invalid worker stream credit");
		Atomics.sub(this.state, 0, 1);
		Atomics.sub(this.state, 1, weight);
		this.wake();
	}
	cancel(): void { Atomics.store(this.state, 3, 1); this.wake(); }
	private wake(): void { Atomics.add(this.state, 2, 1); Atomics.notify(this.state, 2); }
}
