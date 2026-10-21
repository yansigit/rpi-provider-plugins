import { expect, it, vi } from "vitest";
import type { Model, Provider, RefreshModelsContext } from "@earendil-works/pi-ai";
import { NativeCursorCatalog, publishCursorCatalog } from "../../src/rpi/catalog.js";
import { makeModel } from "../helpers/pi-harness.js";
const input = { publicationId: "host-refresh-ticket", allowNetwork: false, force: true };
function fixture() {
	let models: Model<"cursor-sdk">[] = [];
	const update = vi.fn(() => { models = [makeModel()]; });
	const provider = { id: "cursor", getModels: () => models,
		refreshModels: async (context: RefreshModelsContext) => { await publishCursorCatalog(context, [makeModel()], update); },
	} as unknown as Provider;
	return { catalog: new NativeCursorCatalog(provider), update, provider };
}

it("awaits the authoritative publication before releasing dispatch or changing the adapter catalog", async () => {
	const f = fixture();
	let resolve!: (value: unknown) => void;
	const request = vi.fn(() => new Promise<unknown>(done => { resolve = done; }));
	const pending = f.catalog.refresh(input, request, new AbortController().signal);
	let settled = false; const waiting = f.catalog.settled().then(() => { settled = true; });
	await Promise.resolve();
	expect(request).toHaveBeenCalledWith({ port: "providers", action: "publish", providerId: "cursor", publicationId: input.publicationId, models: [makeModel()] });
	expect(f.provider.getModels()).toEqual([]); expect(settled).toBe(false);
	resolve({ revision: 2 });
	expect(await pending).toEqual({ published: true }); await waiting;
	expect(f.provider.getModels()).toEqual([makeModel()]); expect(f.update).toHaveBeenCalledTimes(1);
});

it("refuses late or failed publication without updating private catalog/cache state", async () => {
	const f = fixture();
	await expect(f.catalog.refresh(input, async () => { throw new Error("catalog changed"); }, new AbortController().signal)).rejects.toThrow("catalog changed");
	expect(f.update).not.toHaveBeenCalled();
	await f.catalog.settled();
	await expect(f.catalog.refresh(input, async () => ({ revision: "reserved" }), new AbortController().signal)).rejects.toThrow("acknowledgement");
	expect(f.update).not.toHaveBeenCalled();
});

it("finishes an acknowledged commit after cancellation, but admits no cancelled refresh", async () => {
	const f = fixture(), controller = new AbortController();
	await expect(f.catalog.refresh(input, async () => { controller.abort(); return { revision: 2 }; }, controller.signal)).resolves.toEqual({ published: true });
	expect(f.update).toHaveBeenCalledTimes(1);
	const request = vi.fn();
	await expect(f.catalog.refresh(input, request, controller.signal)).rejects.toThrow();
	expect(request).not.toHaveBeenCalled();
});

it("retains the ordinary Pi publication contract", async () => {
	const update = vi.fn(), publish = vi.fn(async (publication: { update?: () => void }) => { publication.update?.(); return true; });
	await publishCursorCatalog({ publish, allowNetwork: false, signal: new AbortController().signal }, [makeModel()], update);
	expect(publish).toHaveBeenCalledWith({ update }); expect(update).toHaveBeenCalledTimes(1);
});
