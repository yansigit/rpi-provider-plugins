import { createHash } from "node:crypto";
import { isAbsolute, join, resolve } from "node:path";
import { CONFIG_DIR_NAME, getAgentDir as getPiAgentDir } from "@earendil-works/pi-coding-agent";

let nativeAgentDir: string | undefined;

/** Set once before loading the retained extension; one worker belongs to one native runtime. */
export function initializeNativePaths(agentDir: string): void {
	if (typeof agentDir !== "string" || !isAbsolute(agentDir) || agentDir.includes("\0")) throw new Error("Invalid native Cursor storage directory");
	const directory = resolve(agentDir);
	if (nativeAgentDir !== undefined && nativeAgentDir !== directory) throw new Error("Native Cursor storage ownership cannot change within a worker");
	nativeAgentDir = directory;
}
export function getAgentDir(): string { return nativeAgentDir ?? getPiAgentDir(); }
export function getConfigDirName(): string { return nativeAgentDir ? ".rpi" : CONFIG_DIR_NAME; }
export function getNativeSdkWorkspaceRoot(cwd: string): string | undefined {
	return nativeAgentDir ? join(nativeAgentDir, "cursor-sdk", "stores", createHash("sha256").update(cwd).digest("hex")) : undefined;
}
