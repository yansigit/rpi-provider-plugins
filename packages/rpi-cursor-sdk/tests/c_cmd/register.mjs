// Register the `./x.js` -> `./x.ts` resolve hook for `--experimental-transform-types`.
import { register } from "node:module";

register("./resolve.mjs", import.meta.url);
