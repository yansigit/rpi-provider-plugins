import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { runInNewContext } from "node:vm";
import { expect, it } from "vitest";
import type { SendOptions } from "@cursor/sdk";

type Accumulator = {
	apply(update: Parameters<NonNullable<SendOptions["onDelta"]>>[0]["update"]): Promise<void>;
	finalAssistantText(): string | undefined;
};
type AccumulatorConstructor = new (options: Pick<SendOptions, "onDelta">) => Accumulator;

// Execute the pinned SDK accumulator itself; no service, Agent, or fake callback loop.
function installedAccumulator(): AccumulatorConstructor {
	const require = createRequire(import.meta.url);
	const source = readFileSync(join(dirname(require.resolve("@cursor/sdk")), "../esm/index.js"), "utf8");
	const cache: Record<string, Record<string, unknown>> = {};
	const load = Object.assign((name: string): Record<string, unknown> => {
		if (!cache[name]) {
			const start = source.indexOf(`${JSON.stringify(name)}(`);
			const end = source.indexOf(',"./src/', start + name.length + 3);
			if (start < 0 || end < 0) throw new Error("Installed SDK accumulator module contract changed");
			const factory = runInNewContext(`({${source.slice(start, end)}})`)[name];
			cache[name] = {};
			factory({}, cache[name], load);
		}
		return cache[name];
	}, { d(target: object, getters: Record<string, () => unknown>) {
		for (const [name, get] of Object.entries(getters)) Object.defineProperty(target, name, { get });
	} });
	return load("./src/agent/run-interaction-accumulator.ts").e4 as AccumulatorConstructor;
}

it("the installed SDK waits for delta persistence and propagates its rejection", async () => {
	const Accumulator = installedAccumulator();
	let resolve!: () => void;
	let reject!: (error: Error) => void;
	const persistence = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
	let complete = false;
	const accumulator = new Accumulator({ onDelta: () => persistence });
	const pending = accumulator.apply({ type: "text-delta", text: "partial" }).then(() => { complete = true; });
	await Promise.resolve();
	expect(complete).toBe(false);
	expect(accumulator.finalAssistantText()).toBe("partial");
	resolve();
	await pending;
	expect(complete).toBe(true);
	const failure = new Accumulator({ onDelta: () => new Promise<void>((_yes, no) => { reject = no; }) });
	const failed = failure.apply({ type: "text-delta", text: "preserved" });
	const checked = expect(failed).rejects.toThrow("durability failed");
	reject(new Error("durability failed"));
	await checked;
	expect(failure.finalAssistantText()).toBe("preserved");
});
