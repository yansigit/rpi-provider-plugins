// Node --import entry for the fake Cursor SDK.
//   NODE_OPTIONS="--import <abs>/tests/fake-sdk/register.mjs" RPI_CURSOR_FAKE_SCENARIO=<file> ...
// Registers hooks.mjs so that "@cursor/sdk" and "@cursor/sdk/sqlite" resolve to
// the fake while RPI_CURSOR_FAKE_SCENARIO is set. Without that variable the hook
// passes every specifier through unchanged.
import { register } from "node:module";

register("./hooks.mjs", import.meta.url);
