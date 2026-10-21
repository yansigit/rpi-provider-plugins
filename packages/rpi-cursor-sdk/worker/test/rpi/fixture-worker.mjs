import { WorkerPeer, readBootstrap } from "../../dist/rpi/peer.js";

const config = await readBootstrap();
const peer = new WorkerPeer(config, async (method, input, call) => {
	switch (method) {
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
