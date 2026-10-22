// Pi extension fixture: one bridgeable custom tool so the tool-turn scenario can
// execute a Pi-side tool through the adapter's MCP bridge. It is loaded with
// `pi -e` only by scenarios that list it; it performs no I/O.
export default function (pi) {
	pi.registerTool({
		name: "fake_echo",
		label: "Fake echo",
		description: "Echo the given text back verbatim (reference fixture for the fake Cursor SDK).",
		parameters: {
			type: "object",
			properties: { text: { type: "string", description: "Text to echo" } },
			required: ["text"],
		},
		async execute(_toolCallId, params) {
			return { content: [{ type: "text", text: `fake_echo:${params.text}` }], details: {} };
		},
	});
}
