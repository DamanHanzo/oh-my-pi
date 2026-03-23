/**
 * Diff helpers and libedit-backed preview computations.
 */

import { libEditApply } from "@oh-my-pi/pi-natives/libedit";
import * as Diff from "diff";
import { resolveToCwd } from "../tools/path-utils";
import type { DiffError, DiffResult, Operation, PatchInput } from "./types";

function countContentLines(content: string): number {
	const lines = content.split("\n");
	if (lines.length > 1 && lines[lines.length - 1] === "") {
		lines.pop();
	}
	return Math.max(1, lines.length);
}

function formatNumberedDiffLine(prefix: "+" | "-" | " ", lineNum: number, width: number, content: string): string {
	const padded = String(lineNum).padStart(width, " ");
	return `${prefix}${padded}|${content}`;
}

/**
 * Generate a compact line-numbered diff.
 */
export function generateDiffString(oldContent: string, newContent: string, contextLines = 4): DiffResult {
	const parts = Diff.diffLines(oldContent, newContent);
	const output: string[] = [];

	const maxLineNum = Math.max(countContentLines(oldContent), countContentLines(newContent));
	const lineNumWidth = String(maxLineNum).length;

	let oldLineNum = 1;
	let newLineNum = 1;
	let lastWasChange = false;
	let firstChangedLine: number | undefined;

	for (let i = 0; i < parts.length; i++) {
		const part = parts[i];
		const raw = part.value.split("\n");
		if (raw[raw.length - 1] === "") {
			raw.pop();
		}

		if (part.added || part.removed) {
			if (firstChangedLine === undefined) {
				firstChangedLine = newLineNum;
			}

			for (const line of raw) {
				if (part.added) {
					output.push(formatNumberedDiffLine("+", newLineNum, lineNumWidth, line));
					newLineNum++;
				} else {
					output.push(formatNumberedDiffLine("-", oldLineNum, lineNumWidth, line));
					oldLineNum++;
				}
			}
			lastWasChange = true;
			continue;
		}

		const nextPartIsChange = i < parts.length - 1 && (parts[i + 1].added || parts[i + 1].removed);

		if (lastWasChange || nextPartIsChange) {
			let linesToShow = raw;
			let skipStart = 0;
			let skipEnd = 0;

			if (!lastWasChange) {
				skipStart = Math.max(0, raw.length - contextLines);
				linesToShow = raw.slice(skipStart);
			}

			if (!nextPartIsChange && linesToShow.length > contextLines) {
				skipEnd = linesToShow.length - contextLines;
				linesToShow = linesToShow.slice(0, contextLines);
			}

			if (skipStart > 0) {
				output.push(formatNumberedDiffLine(" ", oldLineNum, lineNumWidth, "..."));
				oldLineNum += skipStart;
				newLineNum += skipStart;
			}

			for (const line of linesToShow) {
				output.push(formatNumberedDiffLine(" ", oldLineNum, lineNumWidth, line));
				oldLineNum++;
				newLineNum++;
			}

			if (skipEnd > 0) {
				output.push(formatNumberedDiffLine(" ", oldLineNum, lineNumWidth, "..."));
				oldLineNum += skipEnd;
				newLineNum += skipEnd;
			}
		} else {
			oldLineNum += raw.length;
			newLineNum += raw.length;
		}

		lastWasChange = false;
	}

	return { diff: output.join("\n"), firstChangedLine };
}

/**
 * Generate a unified diff with hunk headers.
 */
export function generateUnifiedDiffString(oldContent: string, newContent: string, contextLines = 3): DiffResult {
	const patch = Diff.structuredPatch("", "", oldContent, newContent, "", "", { context: contextLines });
	const output: string[] = [];
	let firstChangedLine: number | undefined;
	const maxLineNum = Math.max(countContentLines(oldContent), countContentLines(newContent));
	const lineNumWidth = String(maxLineNum).length;

	for (const hunk of patch.hunks) {
		output.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`);
		let oldLine = hunk.oldStart;
		let newLine = hunk.newStart;
		for (const line of hunk.lines) {
			if (line.startsWith("-")) {
				if (firstChangedLine === undefined) firstChangedLine = newLine;
				output.push(formatNumberedDiffLine("-", oldLine, lineNumWidth, line.slice(1)));
				oldLine++;
				continue;
			}
			if (line.startsWith("+")) {
				if (firstChangedLine === undefined) firstChangedLine = newLine;
				output.push(formatNumberedDiffLine("+", newLine, lineNumWidth, line.slice(1)));
				newLine++;
				continue;
			}
			if (line.startsWith(" ")) {
				output.push(formatNumberedDiffLine(" ", oldLine, lineNumWidth, line.slice(1)));
				oldLine++;
				newLine++;
				continue;
			}
			output.push(line);
		}
	}

	return { diff: output.join("\n"), firstChangedLine };
}

export interface ReplaceOptions {
	fuzzy: boolean;
	all: boolean;
	threshold?: number;
}

export interface ReplaceResult {
	content: string;
	count: number;
}

/**
 * Lightweight synchronous replacement helper retained for compatibility.
 */
export function replaceText(content: string, oldText: string, newText: string, options: ReplaceOptions): ReplaceResult {
	if (oldText.length === 0) {
		throw new Error("oldText must not be empty.");
	}

	const occurrences = content.split(oldText).length - 1;
	if (!options.all && occurrences > 1) {
		throw new Error(`Found ${occurrences} occurrences. Add more context lines to disambiguate.`);
	}

	if (occurrences === 0) {
		return { content, count: 0 };
	}

	if (options.all) {
		return {
			content: content.split(oldText).join(newText),
			count: occurrences,
		};
	}

	const idx = content.indexOf(oldText);
	return {
		content: content.slice(0, idx) + newText + content.slice(idx + oldText.length),
		count: 1,
	};
}

function normalizePatchOp(op: string | undefined): Operation {
	if (op === "create" || op === "delete" || op === "update") {
		return op;
	}
	return "update";
}

function mapLibEditError(
	error: unknown,
	displayPath: string,
	absolutePath: string,
	displayRename: string | undefined,
	absoluteRename: string | undefined,
): string {
	let message = error instanceof Error ? error.message : String(error);
	message = message.replaceAll(absolutePath, displayPath);
	if (displayRename && absoluteRename) {
		message = message.replaceAll(absoluteRename, displayRename);
	}
	return message;
}

/**
 * Compute preview diff for replace-mode edit calls.
 */
export async function computeEditDiff(
	path: string,
	oldText: string,
	newText: string,
	cwd: string,
	fuzzy = true,
	all = false,
	threshold?: number,
): Promise<DiffResult | DiffError> {
	if (oldText.length === 0) {
		return { error: "oldText must not be empty." };
	}
	const absolutePath = resolveToCwd(path, cwd);
	const file = Bun.file(absolutePath);
	if (!(await file.exists())) {
		return { error: `File not found: ${path}` };
	}

	try {
		const rawContent = await file.text();
		const { result } = await libEditApply(
			"replace",
			{ path: absolutePath, old_text: oldText, new_text: newText, all },
			[{ path: absolutePath, content: rawContent }],
			{ allowFuzzy: fuzzy, threshold },
		);
		return {
			diff: result.diff ?? "",
			firstChangedLine: result.first_changed_line,
		};
	} catch (error) {
		return { error: mapLibEditError(error, path, absolutePath, undefined, undefined) };
	}
}

/**
 * Compute preview diff for patch-mode edit calls.
 */
export async function computePatchDiff(
	input: PatchInput,
	cwd: string,
	options?: { fuzzyThreshold?: number; allowFuzzy?: boolean },
): Promise<DiffResult | DiffError> {
	const op = normalizePatchOp(input.op);
	const absolutePath = resolveToCwd(input.path, cwd);
	const absoluteRename = input.rename ? resolveToCwd(input.rename, cwd) : undefined;
	const file = Bun.file(absolutePath);
	const seeds: Array<{ path: string; content: string }> = [];
	if (await file.exists()) {
		seeds.push({ path: absolutePath, content: await file.text() });
	}

	try {
		const { result } = await libEditApply(
			"patch",
			{ path: absolutePath, op, rename: absoluteRename, diff: input.diff },
			seeds,
			{
				allowFuzzy: options?.allowFuzzy,
				threshold: options?.fuzzyThreshold,
			},
		);
		return {
			diff: result.diff ?? "",
			firstChangedLine: result.first_changed_line,
		};
	} catch (error) {
		return {
			error: mapLibEditError(error, input.path, absolutePath, input.rename, absoluteRename),
		};
	}
}

/**
 * Compute preview diff for hashline-mode edit calls.
 */
export async function computeHashlineDiff(
	input: { path: string; edits: unknown[]; move?: string },
	cwd: string,
): Promise<DiffResult | DiffError> {
	const absolutePath = resolveToCwd(input.path, cwd);
	const absoluteMove = input.move ? resolveToCwd(input.move, cwd) : undefined;
	const file = Bun.file(absolutePath);
	const seeds: Array<{ path: string; content: string }> = [];
	if (await file.exists()) {
		seeds.push({ path: absolutePath, content: await file.text() });
	}

	try {
		const { result } = await libEditApply(
			"hashline",
			{
				path: absolutePath,
				edits: input.edits,
				...(absoluteMove ? { move: absoluteMove } : {}),
			},
			seeds,
		);
		return {
			diff: result.diff ?? "",
			firstChangedLine: result.first_changed_line,
		};
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		if (
			!absoluteMove &&
			message.includes("No changes made to") &&
			message.includes("edits produced identical content")
		) {
			return {
				error: `No changes would be made to ${input.path}. The edits produce identical content.`,
			};
		}
		return {
			error: mapLibEditError(error, input.path, absolutePath, input.move, absoluteMove),
		};
	}
}
