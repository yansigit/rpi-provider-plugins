// Node --import entry for the fake Cursor SDK.
//   NODE_OPTIONS="--import <abs>/tests/c-cloud/fake-sdk/register.mjs" RPI_CURSOR_FAKE_SCENARIO=<file> ...
// Registers hooks.mjs so that "@cursor/sdk" and "@cursor/sdk/sqlite" resolve to
// the fake while RPI_CURSOR_FAKE_SCENARIO is set. Without that variable the hook
// passes every specifier through unchanged.
//
// RPI_CURSOR_FAKE_CLOCK=<ISO-8601> freezes the wall clock for the whole process
// (Date.now and `new Date()`), so lifecycle timestamps and duration fields are
// identical between the Pi reference and the rpi worker. It is inert otherwise.
import { register } from "node:module";

register("./hooks.mjs", import.meta.url);

const frozen = process.env.RPI_CURSOR_FAKE_CLOCK;
if (frozen) {
	const millis = new Date(frozen).getTime();
	if (!Number.isFinite(millis)) throw new Error(`RPI_CURSOR_FAKE_CLOCK is not an ISO-8601 timestamp: ${frozen}`);
	const OriginalDate = Date;
	globalThis.Date = class extends OriginalDate {
		constructor(...args) {
			super(...(args.length ? args : [millis]));
		}
		static now() {
			return millis;
		}
	};
}
