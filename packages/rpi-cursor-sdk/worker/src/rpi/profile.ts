/** Set once by a minimal-host runtime; read where SDK agents are built. */
let minimal = false;
export function setMinimalHost(): void { minimal = true; }
export function isMinimalHost(): boolean { return minimal; }
