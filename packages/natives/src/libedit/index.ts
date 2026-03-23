/**
 * Native libedit wrappers.
 */

import { native } from "../native";
import type {
	LibEditFileSeed,
	LibEditMethodInfo,
	LibEditOperation,
	LibEditOptions,
	LibEditResult,
	NativeLibEditApplyOutput,
} from "./types";

export type {
	LibEditFileChange,
	LibEditFileSeed,
	LibEditMethodInfo,
	LibEditOperation,
	LibEditOptions,
	LibEditResult,
	NativeLibEditApplyOutput,
} from "./types";

export function libEditListMethods(): LibEditMethodInfo[] {
	return native.libEditListMethods();
}

export async function libEditApply(
	methodName: string,
	input: unknown,
	files: LibEditFileSeed[],
	options?: LibEditOptions,
): Promise<{ result: LibEditResult; operations: LibEditOperation[] }> {
	const raw = (await native.libEditApply(
		methodName,
		JSON.stringify(input),
		files,
		options,
	)) as NativeLibEditApplyOutput;
	const parsed = JSON.parse(raw.resultJson) as Partial<LibEditResult>;
	return {
		result: {
			...parsed,
			change: parsed.change as LibEditResult["change"],
			message: parsed.message ?? "",
			changes: parsed.changes ?? [],
			warnings: parsed.warnings ?? [],
		},
		operations: raw.operations,
	};
}

export function libEditFormatHashLines(content: string, startLine?: number): string {
	return native.libEditFormatHashLines(content, startLine);
}

export function libEditComputeLineHash(lineNumber: number, line: string): string {
	return native.libEditComputeLineHash(lineNumber, line);
}
