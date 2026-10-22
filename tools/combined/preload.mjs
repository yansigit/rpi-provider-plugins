// Deterministic clock and randomness for the Pi side of a parity run.
// Loaded as `node --import tools/parity-rpc/preload.mjs <pi cli>`; Pi's own
// code is not modified.
//
//   Date.now() and `new Date()` (no arguments) advance by 1 ms per read from a
//   fixed start, so a run's clock depends only on its call sequence.
//   Math.random, crypto.randomUUID, crypto.randomBytes and the global
//   crypto.getRandomValues draw from one fixed-seed generator (mulberry32).
//
// Not covered: performance.now(), `Date()` called as a function, timers,
// node:crypto's getRandomValues export, and entropy taken outside these APIs.
// The normalizer handles the timing fields; README.md lists the limits.
import crypto from "node:crypto";
import { syncBuiltinESMExports } from "node:module";

const START_MS = Date.UTC(2026, 0, 1, 0, 0, 0);
const SEED = 0x5eedc0de;

let ticks = 0;
const nextNowMs = () => START_MS + ticks++;

const RealDate = Date;
class ParityDate extends RealDate {
  constructor(...args) {
    if (args.length === 0) super(nextNowMs());
    else super(...args);
  }

  static now() {
    return nextNowMs();
  }
}
globalThis.Date = ParityDate;

let state = SEED >>> 0;
function nextUint32() {
  state = (state + 0x6d2b79f5) >>> 0;
  let t = state;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return (t ^ (t >>> 14)) >>> 0;
}

function nextBytes(length) {
  const bytes = Buffer.alloc(length);
  for (let offset = 0; offset < length; offset += 4) {
    const word = nextUint32();
    for (let k = 0; k < 4 && offset + k < length; k++) {
      bytes[offset + k] = (word >>> (8 * k)) & 0xff;
    }
  }
  return bytes;
}

function randomUUID() {
  const bytes = nextBytes(16);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function getRandomValues(array) {
  const view = new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
  view.set(nextBytes(view.byteLength));
  return array;
}

Math.random = () => nextUint32() / 4294967296;

function randomBytes(size, callback) {
  const buffer = nextBytes(size);
  if (typeof callback === "function") {
    process.nextTick(callback, null, buffer);
    return undefined;
  }
  return buffer;
}

// node:crypto's getRandomValues export is a non-configurable getter, so only
// the global Web Crypto object and the configurable exports are replaced.
crypto.randomUUID = randomUUID;
crypto.randomBytes = randomBytes;
Object.defineProperty(globalThis.crypto, "randomUUID", { value: randomUUID, configurable: true, writable: true });
Object.defineProperty(globalThis.crypto, "getRandomValues", { value: getRandomValues, configurable: true, writable: true });
// Named ESM imports of node:crypto read the live exports only after a sync.
syncBuiltinESMExports();
