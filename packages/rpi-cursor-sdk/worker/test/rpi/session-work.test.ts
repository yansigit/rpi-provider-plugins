import { expect, it } from "vitest";
import { NativeSessionWork, bindNativeSessionCallback, trackNativeSessionWork } from "../../src/rpi/session-work.js";
import { nativeSessionFixture } from "../helpers/native-session.js";

function gate() {
	let resolve!: () => void;
	const promise = new Promise<void>(done => { resolve = done; });
	return { promise, resolve };
}

it("retains an origin until its last holder releases and awaits the one host acknowledgement", async () => {
	const f = await nativeSessionFixture(), acknowledgement = gate();
	const one = f.origin.retain(), two = f.origin.retain();
	await f.origin.release(); await f.origin.release(); await one(); await one();
	expect(f.origin.view().id).toBe("native-session");
	const host = f.request.getMockImplementation()!;
	f.request.mockImplementation(async input => { if (input.action === "release") await acknowledgement.promise; return host(input); });
	let released = false;
	const pending = two().then(() => { released = true; });
	const repeated = two();
	await Promise.resolve(); expect(released).toBe(false);
	expect(() => f.origin.retain()).toThrow("closed");
	acknowledgement.resolve(); await pending; await repeated;
	expect(f.request.mock.calls.filter(([input]) => input.action === "release")).toHaveLength(1);
});

it("late SDK work and a detached abandonment callback keep their original branch and manager context", async () => {
	const f = await nativeSessionFixture(), sdk = gate(), write = gate();
	await f.origin.append("before", {});
	const work = new NativeSessionWork(f.origin);
	let abandon!: () => Promise<void>;
	await f.manager.run(f.origin, () => work.run(async () => {
		trackNativeSessionWork(sdk.promise);
		abandon = bindNativeSessionCallback(async () => {
			await write.promise;
			expect(f.manager.origin()).toBe(f.origin);
			await f.origin.append("late-usage", { terminal: true }, true);
		});
		return "stream complete";
	}));
	await f.origin.release();
	const next = await f.branch(null);
	await next.append("new-branch", {});
	const active = f.activeLeaf;
	const callback = abandon(); sdk.resolve();
	let settled = false; void work.settled.then(() => { settled = true; });
	await Promise.resolve(); await Promise.resolve(); expect(settled).toBe(false);
	write.resolve(); await callback; await work.settled;
	expect(f.activeLeaf).toBe(active);
	expect(f.entries.at(-1)).toMatchObject({ customType: "late-usage", parentId: f.entries[0].id });
	expect(() => f.origin.view()).toThrow("closed");
	await expect(abandon()).rejects.toThrow("closed");
	await next.release();
});

it("releases failed and cancelled work, surfaces background failure, and does not exhaust host origins", async () => {
	const f = await nativeSessionFixture();
	await f.origin.release();
	for (let index = 0; index < 140; index++) {
		const origin = await f.capture(), work = new NativeSessionWork(origin);
		await origin.release();
		await work.run(async () => { await origin.sync(); });
		await work.settled;
		expect(() => origin.view()).toThrow("closed");
	}
	const origin = await f.capture(), work = new NativeSessionWork(origin);
	await origin.release();
	await expect(work.run(async () => {
		trackNativeSessionWork(Promise.reject(new Error("billing write failed")));
		throw new Error("caller cancelled");
	})).rejects.toThrow("caller cancelled");
	await expect(work.settled).rejects.toMatchObject({ errors: [new Error("billing write failed")] });
	expect(() => origin.view()).toThrow("closed");
	expect(f.request.mock.calls.filter(([input]) => input.action === "release")).toHaveLength(142);
});
