// Pi extension fixture for the c-tools differential: one bridgeable tool that
// always fails, so the MCP bridge failure path can be compared between the pinned
// adapter and the rpi worker. It is loaded with `pi -e` only by the scenario that
// lists it; it performs no I/O.
export default function (pi) {
	pi.registerTool({
		name: "fake_fail",
		label: "Fake fail",
		description: "Always throws (reference fixture for the fake Cursor SDK).",
		parameters: {
			type: "object",
			properties: { text: { type: "string", description: "Text to reject" } },
			required: ["text"],
		},
		async execute() {
			throw new Error("fake_fail: intentional bridge failure");
		},
	});
}
