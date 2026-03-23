/**
 * Hashline helpers.
 *
 * The hashing/formatting primitives are delegated to native libedit.
 * A lightweight in-memory edit applicator is kept for compatibility with
 * callers that still import `applyHashlineEdits` from this module.
 */

import { libEditComputeLineHash, libEditFormatHashLines } from "@oh-my-pi/pi-natives/libedit";
import type { HashMismatch } from "./types";

export interface NoopHashlineEdit {
	editIndex: number;
	loc: string;
	current: string;
}

export interface HashlineApplyResult {
	lines: string;
	firstChangedLine?: number;
	warnings?: string[];
	noopEdits?: NoopHashlineEdit[];
}

const TAG_RE = /^\s*[>+-]*\s*(\d+)\s*#\s*([ZPMQVRWSNKTXJBYH]{2})/;

export interface Anchor {
	line: number;
	hash: string;
}

export function computeLineHash(idx: number, line: string): string {
	return libEditComputeLineHash(idx, line);
}

export function formatLineTag(line: number, text: string): string {
	return `${line}#${computeLineHash(line, text)}`;
}

export function formatHashLines(text: string, startLine = 1): string {
	return libEditFormatHashLines(text, startLine);
}

export function parseTag(ref: string): Anchor {
	const match = TAG_RE.exec(ref);
	if (!match) {
		throw new Error(`Invalid line reference "${ref}". Expected format "LINE#ID" (e.g. "5#ZP").`);
	}
	const line = Number(match[1]);
	if (!Number.isInteger(line) || line < 1) {
		throw new Error(`Line number must be >= 1, got ${line} in "${ref}".`);
	}
	return { line, hash: match[2] };
}

export class HashlineMismatchError extends Error {
	constructor(
		public readonly mismatches: HashMismatch[],
		fileLines: string[],
	) {
		super(formatHashMismatchError(mismatches, fileLines));
		this.name = "HashlineMismatchError";
	}
}

export function formatHashMismatchError(mismatches: HashMismatch[], fileLines: string[]): string {
	const mismatchLines = new Set(mismatches.map(mismatch => mismatch.line));
	const out: string[] = [
		`${mismatches.length} line(s) changed since last read. Use the updated LINE#ID references shown below (>>> marks changed lines).`,
		"",
	];
	for (let i = 0; i < fileLines.length; i++) {
		const lineNo = i + 1;
		const tag = `${lineNo}#${computeLineHash(lineNo, fileLines[i])}`;
		const marker = mismatchLines.has(lineNo) ? ">>>" : "   ";
		out.push(`${marker} ${tag}:${fileLines[i]}`);
	}
	return out.join("\n");
}

export function validateLineRef(ref: Anchor, fileLines: string[]): void {
	if (ref.line < 1 || ref.line > fileLines.length) {
		throw new Error(`Line ${ref.line} does not exist (file has ${fileLines.length} lines).`);
	}
	const actualHash = computeLineHash(ref.line, fileLines[ref.line - 1] ?? "");
	if (actualHash !== ref.hash) {
		throw new HashlineMismatchError(
			[
				{
					line: ref.line,
					expected: ref.hash,
					actual: actualHash,
				},
			],
			fileLines,
		);
	}
}

export interface HashlineStreamOptions {
	startLine?: number;
	maxChunkLines?: number;
	maxChunkBytes?: number;
}

function isReadableStream(value: unknown): value is ReadableStream<Uint8Array> {
	return (
		typeof value === "object" &&
		value !== null &&
		"getReader" in value &&
		typeof (value as { getReader?: unknown }).getReader === "function"
	);
}

async function* bytesFromReadableStream(stream: ReadableStream<Uint8Array>): AsyncGenerator<Uint8Array> {
	const reader = stream.getReader();
	try {
		while (true) {
			const { done, value } = await reader.read();
			if (done) return;
			if (value) yield value;
		}
	} finally {
		reader.releaseLock();
	}
}

/**
 * Stream hashline formatting from UTF-8 chunks.
 */
export async function* streamHashLinesFromUtf8(
	source: ReadableStream<Uint8Array> | AsyncIterable<Uint8Array>,
	options: HashlineStreamOptions = {},
): AsyncGenerator<string> {
	const startLine = options.startLine ?? 1;
	const maxChunkLines = options.maxChunkLines ?? 200;
	const maxChunkBytes = options.maxChunkBytes ?? 64 * 1024;
	const decoder = new TextDecoder("utf-8");
	const chunks = isReadableStream(source) ? bytesFromReadableStream(source) : source;
	let lineNum = startLine;
	let pending = "";
	let sawAnyText = false;
	let endedWithNewline = false;
	let outLines: string[] = [];
	let outBytes = 0;

	const flush = (): string | undefined => {
		if (outLines.length === 0) return undefined;
		const chunk = outLines.join("\n");
		outLines = [];
		outBytes = 0;
		return chunk;
	};

	const pushLine = (line: string): string[] => {
		const formatted = `${lineNum}#${computeLineHash(lineNum, line)}:${line}`;
		lineNum++;

		const chunksToYield: string[] = [];
		const sepBytes = outLines.length === 0 ? 0 : 1;
		const lineBytes = Buffer.byteLength(formatted, "utf-8");

		if (
			outLines.length > 0 &&
			(outLines.length >= maxChunkLines || outBytes + sepBytes + lineBytes > maxChunkBytes)
		) {
			const flushed = flush();
			if (flushed) chunksToYield.push(flushed);
		}

		outLines.push(formatted);
		outBytes += (outLines.length === 1 ? 0 : 1) + lineBytes;

		if (outLines.length >= maxChunkLines || outBytes >= maxChunkBytes) {
			const flushed = flush();
			if (flushed) chunksToYield.push(flushed);
		}

		return chunksToYield;
	};

	const consumeText = (text: string): string[] => {
		if (text.length === 0) return [];
		sawAnyText = true;
		pending += text;
		const chunksToYield: string[] = [];
		while (true) {
			const idx = pending.indexOf("\n");
			if (idx === -1) break;
			const line = pending.slice(0, idx);
			pending = pending.slice(idx + 1);
			endedWithNewline = true;
			chunksToYield.push(...pushLine(line));
		}
		if (pending.length > 0) endedWithNewline = false;
		return chunksToYield;
	};

	for await (const chunk of chunks) {
		for (const out of consumeText(decoder.decode(chunk, { stream: true }))) {
			yield out;
		}
	}

	for (const out of consumeText(decoder.decode())) {
		yield out;
	}

	if (!sawAnyText) {
		for (const out of pushLine("")) {
			yield out;
		}
	} else if (pending.length > 0 || endedWithNewline) {
		for (const out of pushLine(pending)) {
			yield out;
		}
	}

	const last = flush();
	if (last) yield last;
}

/**
 * Stream hashline formatting from text lines.
 */
export async function* streamHashLinesFromLines(
	lines: AsyncIterable<string> | Iterable<string>,
	options: HashlineStreamOptions = {},
): AsyncGenerator<string> {
	const startLine = options.startLine ?? 1;
	const maxChunkLines = options.maxChunkLines ?? 200;
	const maxChunkBytes = options.maxChunkBytes ?? 64 * 1024;
	let lineNum = startLine;
	let outLines: string[] = [];
	let outBytes = 0;

	const flush = (): string | undefined => {
		if (outLines.length === 0) return undefined;
		const chunk = outLines.join("\n");
		outLines = [];
		outBytes = 0;
		return chunk;
	};

	for await (const line of lines) {
		const formatted = `${lineNum}#${computeLineHash(lineNum, line)}:${line}`;
		lineNum++;
		const lineBytes = Buffer.byteLength(formatted, "utf-8");
		if (outLines.length > 0 && (outLines.length >= maxChunkLines || outBytes + 1 + lineBytes > maxChunkBytes)) {
			const chunk = flush();
			if (chunk) yield chunk;
		}
		outLines.push(formatted);
		outBytes += (outLines.length === 1 ? 0 : 1) + lineBytes;
		if (outLines.length >= maxChunkLines || outBytes >= maxChunkBytes) {
			const chunk = flush();
			if (chunk) yield chunk;
		}
	}

	const last = flush();
	if (last) yield last;
}

export interface CompactHashlineDiffPreview {
	preview: string;
	addedLines: number;
	removedLines: number;
}

export interface CompactHashlineDiffOptions {
	maxUnchangedRun?: number;
	maxAdditionRun?: number;
	maxDeletionRun?: number;
	maxOutputLines?: number;
}

const NUMBERED_DIFF_LINE_RE = /^([ +-])(\s*\d+)\|(.*)$/;
const HASHLINE_PREVIEW_PLACEHOLDER = "   ";

type DiffRunKind = " " | "+" | "-" | "meta";
type DiffRun = { kind: DiffRunKind; lines: string[] };

interface ParsedNumberedDiffLine {
	kind: " " | "+" | "-";
	lineNumber: number;
	lineWidth: number;
	content: string;
	raw: string;
}

interface CompactPreviewCounters {
	oldLine?: number;
	newLine?: number;
}

function parseNumberedDiffLine(line: string): ParsedNumberedDiffLine | undefined {
	const match = NUMBERED_DIFF_LINE_RE.exec(line);
	if (!match) return undefined;
	const kind = match[1];
	if (kind !== " " && kind !== "+" && kind !== "-") return undefined;
	const lineField = match[2];
	const lineNumber = Number(lineField.trim());
	if (!Number.isInteger(lineNumber)) return undefined;
	return { kind, lineNumber, lineWidth: lineField.length, content: match[3], raw: line };
}

function syncOldLineCounters(counters: CompactPreviewCounters, lineNumber: number): void {
	if (counters.oldLine === undefined || counters.newLine === undefined) {
		counters.oldLine = lineNumber;
		counters.newLine = lineNumber;
		return;
	}
	const delta = lineNumber - counters.oldLine;
	counters.oldLine = lineNumber;
	counters.newLine += delta;
}

function syncNewLineCounters(counters: CompactPreviewCounters, lineNumber: number): void {
	if (counters.oldLine === undefined || counters.newLine === undefined) {
		counters.oldLine = lineNumber;
		counters.newLine = lineNumber;
		return;
	}
	const delta = lineNumber - counters.newLine;
	counters.oldLine += delta;
	counters.newLine = lineNumber;
}

function formatCompactHashlineLine(kind: " " | "+", lineNumber: number, width: number, content: string): string {
	const padded = String(lineNumber).padStart(width, " ");
	return `${kind}${padded}#${computeLineHash(lineNumber, content)}|${content}`;
}

function formatCompactRemovedLine(lineNumber: number, width: number, content: string): string {
	const padded = String(lineNumber).padStart(width, " ");
	return `-${padded}${HASHLINE_PREVIEW_PLACEHOLDER}|${content}`;
}

function formatCompactPreviewLine(line: string, counters: CompactPreviewCounters): { kind: DiffRunKind; text: string } {
	const parsed = parseNumberedDiffLine(line);
	if (!parsed) return { kind: "meta", text: line };

	if (parsed.content === "...") {
		if (parsed.kind === "+") {
			syncNewLineCounters(counters, parsed.lineNumber);
		} else {
			syncOldLineCounters(counters, parsed.lineNumber);
		}
		return { kind: parsed.kind, text: parsed.raw };
	}

	switch (parsed.kind) {
		case "+":
			syncNewLineCounters(counters, parsed.lineNumber);
			if (counters.newLine === undefined) return { kind: "+", text: parsed.raw };
			{
				const text = formatCompactHashlineLine("+", counters.newLine, parsed.lineWidth, parsed.content);
				counters.newLine += 1;
				return { kind: "+", text };
			}
		case "-":
			syncOldLineCounters(counters, parsed.lineNumber);
			counters.oldLine = parsed.lineNumber + 1;
			return { kind: "-", text: formatCompactRemovedLine(parsed.lineNumber, parsed.lineWidth, parsed.content) };
		case " ":
			syncOldLineCounters(counters, parsed.lineNumber);
			if (counters.newLine === undefined) return { kind: " ", text: parsed.raw };
			{
				const text = formatCompactHashlineLine(" ", counters.newLine, parsed.lineWidth, parsed.content);
				counters.oldLine = parsed.lineNumber + 1;
				counters.newLine += 1;
				return { kind: " ", text };
			}
	}
}

function splitDiffRuns(lines: string[]): DiffRun[] {
	const runs: DiffRun[] = [];
	const counters: CompactPreviewCounters = {};
	for (const line of lines) {
		const formatted = formatCompactPreviewLine(line, counters);
		const prev = runs[runs.length - 1];
		if (prev && prev.kind === formatted.kind) {
			prev.lines.push(formatted.text);
		} else {
			runs.push({ kind: formatted.kind, lines: [formatted.text] });
		}
	}
	return runs;
}

function collapseFromStart(lines: string[], maxLines: number, label: string): string[] {
	if (lines.length <= maxLines) return lines;
	const hidden = lines.length - maxLines;
	return [...lines.slice(0, maxLines), ` ... ${hidden} more ${label} lines`];
}

function collapseFromEnd(lines: string[], maxLines: number, label: string): string[] {
	if (lines.length <= maxLines) return lines;
	const hidden = lines.length - maxLines;
	return [` ... ${hidden} more ${label} lines`, ...lines.slice(-maxLines)];
}

function collapseFromMiddle(lines: string[], maxLines: number, label: string): string[] {
	if (lines.length <= maxLines * 2) return lines;
	const hidden = lines.length - maxLines * 2;
	return [...lines.slice(0, maxLines), ` ... ${hidden} more ${label} lines`, ...lines.slice(-maxLines)];
}

export function buildCompactHashlineDiffPreview(
	diff: string,
	options: CompactHashlineDiffOptions = {},
): CompactHashlineDiffPreview {
	const maxUnchangedRun = options.maxUnchangedRun ?? 2;
	const maxAdditionRun = options.maxAdditionRun ?? 2;
	const maxDeletionRun = options.maxDeletionRun ?? 2;
	const maxOutputLines = options.maxOutputLines ?? 16;

	const inputLines = diff.length === 0 ? [] : diff.split("\n");
	const runs = splitDiffRuns(inputLines);
	const out: string[] = [];
	let addedLines = 0;
	let removedLines = 0;

	for (let runIndex = 0; runIndex < runs.length; runIndex++) {
		const run = runs[runIndex];
		switch (run.kind) {
			case "meta":
				out.push(...run.lines);
				break;
			case "+":
				addedLines += run.lines.length;
				out.push(...collapseFromStart(run.lines, maxAdditionRun, "added"));
				break;
			case "-":
				removedLines += run.lines.length;
				out.push(...collapseFromStart(run.lines, maxDeletionRun, "removed"));
				break;
			case " ":
				if (runIndex === 0) {
					out.push(...collapseFromEnd(run.lines, maxUnchangedRun, "unchanged"));
				} else if (runIndex === runs.length - 1) {
					out.push(...collapseFromStart(run.lines, maxUnchangedRun, "unchanged"));
				} else {
					out.push(...collapseFromMiddle(run.lines, maxUnchangedRun, "unchanged"));
				}
				break;
		}
	}

	if (out.length > maxOutputLines) {
		const hidden = out.length - maxOutputLines;
		return {
			preview: [...out.slice(0, maxOutputLines), ` ... ${hidden} more preview lines`].join("\n"),
			addedLines,
			removedLines,
		};
	}

	return { preview: out.join("\n"), addedLines, removedLines };
}
