import { NativeJournals } from "../../dist/rpi/journal.js";
import { WorkerPeer, readBootstrap } from "../../dist/rpi/peer.js";

const config = await readBootstrap();
const peer = new WorkerPeer(config, async (method, input, call) => {
	switch (method) {
		case "journalSequence": {
			const journals = new NativeJournals(request => call.host("fixture.journal", { index: input.index, request }));
			const outcomes = [];
			for (const record of input.records) {
				try {
					if (input.kind === "cloud") await journals.appendCloud(record);
					else await journals.appendUsage(record, () => {});
					outcomes.push(true);
				} catch (error) { outcomes.push(error.message); }
			}
			return outcomes;
		}
		case "journal": {
			const journals = new NativeJournals(request => call.host("fixture.journal", request));
			if (input.kind === "cloud") {
				await journals.appendCloud(input.record);
				return await journals.readCloud();
			}
			if (input.record) await journals.appendUsage(input.record, rows => {
				if (new Set(rows.map(row => row.id)).size !== rows.length) throw new Error("duplicate fixture record");
			});
			return await journals.readUsage();
		}
		case "echo": return input;
		case "nested": return await call.host("fixture.host", input);
		case "burst":
			for (let i = 0; i < input.count; i++) call.emit({ i, text: "x".repeat(input.bytes ?? 0) });
			return { count: input.count };
		case "hold":
			await new Promise(resolve => call.signal.addEventListener("abort", resolve, { once: true }));
			return { cancelled: true };
		case "crash":
			call.emit({ partial: "preserved" });
			setTimeout(() => process.exit(7), 25);
			await new Promise(() => {});
			break;
		case "stdio": process.stdout.write("stdout is not IPC\n"); process.stderr.write("stderr is not IPC\n"); return true;
		default: throw new Error("fixture secret must not escape");
	}
});
await peer.ready;
await peer.disconnected;
await peer.close();
