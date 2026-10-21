import type { AuthResult, Model, Provider, RefreshModelsContext } from "@earendil-works/pi-ai";

type Publisher = (models: readonly Model<"cursor-sdk">[], update: () => void) => Promise<boolean>;
const publishers = new WeakMap<RefreshModelsContext, Publisher>();
const authentications = new WeakMap<RefreshModelsContext, AuthResult | undefined>();

/** Each native refresh resolves through its current host callback, never a previous event context. */
export function nativeCatalogAuth(context: RefreshModelsContext): { value: AuthResult | undefined } | undefined {
	return authentications.has(context) ? { value: authentications.get(context) } : undefined;
}

/** Native publication acknowledges the candidate before changing adapter-private catalog state. */
export function publishCursorCatalog(context: RefreshModelsContext, models: readonly Model<"cursor-sdk">[], update: () => void): Promise<boolean> {
	const publish = publishers.get(context);
	return publish ? publish(models, update) : context.publish({ update });
}

type Request = (request: Record<string, unknown>) => Promise<unknown>;
export type NativeRefresh = { publicationId: string; allowNetwork: boolean; force: boolean };
export class NativeCursorCatalog {
	private provider: Provider;
	private pending: Promise<void> = Promise.resolve();
	constructor(provider: Provider) { this.provider = provider; }
	/** Dispatch waits here so it cannot observe half of a cross-process publication. */
	async settled(): Promise<void> {
		for (;;) { const pending = this.pending; await pending; if (pending === this.pending) return; }
	}
	async refresh(input: NativeRefresh, request: Request, signal: AbortSignal, auth?: AuthResult): Promise<unknown> {
		if (typeof input.publicationId !== "string" || !input.publicationId || typeof input.allowNetwork !== "boolean" || typeof input.force !== "boolean") {
			throw new Error("Invalid native Cursor catalog refresh");
		}
		signal.throwIfAborted();
		let published = false;
		const context: RefreshModelsContext = {
			allowNetwork: input.allowNetwork, force: input.force, signal,
			publish: async () => { throw new Error("Native Cursor catalog requires candidate publication"); },
		};
		authentications.set(context, auth);
		publishers.set(context, (models, update) => {
			// Pi serializes publication, while overlapping discovery can be superseded independently.
			const operation = this.pending.then(async () => {
				if (published) throw new Error("Cursor catalog was published twice in one refresh");
				signal.throwIfAborted();
				// Once admitted, the Rust port retains this commit through cancellation.
				const receipt = await request({ port: "providers", action: "publish", providerId: this.provider.id,
					publicationId: input.publicationId, models });
				if (!receipt || typeof receipt !== "object" || !("revision" in receipt) ||
					!Number.isSafeInteger(receipt.revision) || (receipt.revision as number) <= 0) {
					throw new Error("Invalid native catalog acknowledgement; refresh before using the catalog");
				}
				update(); published = true;
				return true;
			});
			this.pending = operation.then(() => {}, () => {});
			return operation;
		});
		try {
			await this.provider.refreshModels?.(context);
			return published ? { published: true } : { models: this.provider.getModels() };
		} finally { publishers.delete(context); authentications.delete(context); }
	}
}
