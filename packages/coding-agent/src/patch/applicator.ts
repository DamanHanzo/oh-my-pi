/**
 * libedit-backed patch application helpers.
 *
 * This module keeps the existing `applyPatch`/`previewPatch` contract while
 * delegating patch semantics to the native libedit engine.
 */

import * as fs from "node:fs/promises";
import { type LibEditResult, libEditApply } from "@oh-my-pi/pi-natives/libedit";
import { resolveToCwd } from "../tools/path-utils";
import type { ApplyPatchOptions, ApplyPatchResult, FileChange, FileSystem, Operation, PatchInput } from "./types";
import { ApplyPatchError, ParseError } from "./types";

export const defaultFileSystem: FileSystem = {
	async exists(path: string): Promise<boolean> {
		return Bun.file(path).exists();
	},
	async read(path: string): Promise<string> {
		return fs.readFile(path, "utf8");
	},
	async readBinary(path: string): Promise<Uint8Array> {
		return new Uint8Array(await Bun.file(path).arrayBuffer());
	},
	async write(path: string, content: string): Promise<void> {
		await Bun.write(path, content);
	},
	async delete(path: string): Promise<void> {
		await fs.unlink(path);
	},
	async mkdir(path: string): Promise<void> {
		await fs.mkdir(path, { recursive: true });
	},
};

function normalizeOperation(op: PatchInput["op"]): Operation {
	if (op === "create" || op === "delete" || op === "update") {
		return op;
	}
	return "update";
}

function normalizePatchPayload(input: PatchInput, cwd: string): PatchInput {
	const op = normalizeOperation(input.op);
	const path = resolveToCwd(input.path, cwd);
	const rename = input.rename ? resolveToCwd(input.rename, cwd) : undefined;
	return { path, op, rename, diff: input.diff };
}

function normalizeLibEditError(error: unknown): Error {
	const message = error instanceof Error ? error.message : String(error);
	const lineMatch = /^Line (\d+):\s*(.+)$/.exec(message);
	if (lineMatch) {
		return new ParseError(lineMatch[2] ?? message, Number(lineMatch[1]));
	}
	if (
		message.includes("Diff contains no hunks") ||
		message.includes("Expected hunk") ||
		message.includes("Invalid line reference")
	) {
		return new ParseError(message);
	}
	return new ApplyPatchError(message);
}

function toFileChange(change: LibEditResult["change"]): FileChange {
	return {
		type: change.op,
		path: change.path,
		newPath: change.new_path,
		oldContent: change.old_content,
		newContent: change.new_content,
	};
}

async function buildSeeds(
	path: string,
	rename: string | undefined,
	fsApi: FileSystem,
): Promise<Array<{ path: string; content: string }>> {
	const seeds: Array<{ path: string; content: string }> = [];
	if (await fsApi.exists(path)) {
		seeds.push({ path, content: await fsApi.read(path) });
	}
	if (rename && rename !== path && (await fsApi.exists(rename))) {
		seeds.push({ path: rename, content: await fsApi.read(rename) });
	}
	return seeds;
}

async function applyOperations(
	operations: Array<{ kind: "write" | "delete"; path: string; content?: string }>,
	fsApi: FileSystem,
): Promise<void> {
	for (const operation of operations) {
		if (operation.kind === "write") {
			if (operation.content === undefined) {
				throw new ApplyPatchError(`libedit write operation missing content for ${operation.path}`);
			}
			await fsApi.write(operation.path, operation.content);
			continue;
		}
		try {
			await fsApi.delete(operation.path);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code !== "ENOENT") {
				throw error;
			}
		}
	}
}

/**
 * Apply a patch operation via libedit.
 */
export async function applyPatch(input: PatchInput, options: ApplyPatchOptions): Promise<ApplyPatchResult> {
	const fsApi = options.fs ?? defaultFileSystem;
	const normalized = normalizePatchPayload(input, options.cwd);
	try {
		const { result, operations } = await libEditApply(
			"patch",
			{
				path: normalized.path,
				op: normalized.op,
				rename: normalized.rename,
				diff: normalized.diff,
			},
			await buildSeeds(normalized.path, normalized.rename, fsApi),
			{
				allowFuzzy: options.allowFuzzy,
				threshold: options.fuzzyThreshold,
			},
		);
		if (!options.dryRun) {
			await applyOperations(operations, fsApi);
		}
		return {
			change: toFileChange(result.change),
			warnings: result.warnings,
		};
	} catch (error) {
		throw normalizeLibEditError(error);
	}
}

/**
 * Preview a patch operation without writing changes.
 */
export async function previewPatch(input: PatchInput, options: ApplyPatchOptions): Promise<ApplyPatchResult> {
	return applyPatch(input, { ...options, dryRun: true });
}
