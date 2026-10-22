// Deterministic clock, randomness and IDs for the C-USAGE reference runs.
// Loaded with `node --require` so it also reaches `node:crypto`'s ESM named
// bindings (the usage ledger imports { randomUUID } directly).
"use strict";

const FIXED_MS = 1_729_000_000_123;

const OriginalDate = Date;
class FixedDate extends OriginalDate {
	constructor(...args) {
		super(...(args.length ? args : [FIXED_MS]));
	}
	static now() {
		return FIXED_MS;
	}
}
globalThis.Date = FixedDate;

const crypto = require("node:crypto");
let uuidCounter = 0;
crypto.randomUUID = () => {
	uuidCounter += 1;
	return `00000000-0000-4000-8000-${String(uuidCounter).padStart(12, "0")}`;
};

let randomCounter = 0;
crypto.randomBytes = (size) => {
	const out = Buffer.alloc(size);
	for (let i = 0; i < size; i += 1) {
		randomCounter += 1;
		out[i] = randomCounter & 0xff;
	}
	return out;
};
crypto.randomInt = (min, max) => {
	if (max === undefined) return min & 0x7fffffff;
	randomCounter += 1;
	return min + (randomCounter % (max - min));
};
