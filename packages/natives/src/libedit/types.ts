/**
 * Types for native libedit operations.
 */

export interface LibEditMethodInfo {
	name: string;
	prompt: string;
	schema: string;
	grammar?: string;
}

export interface LibEditOptions {
	allowFuzzy?: boolean;
	threshold?: number;
}

export interface LibEditFileSeed {
	path: string;
	content: string;
}

export interface LibEditOperation {
	kind: "write" | "delete";
	path: string;
	content?: string;
}

export interface LibEditFileChange {
	op: "create" | "update" | "delete";
	path: string;
	new_path?: string;
	old_content?: string;
	new_content?: string;
}

export interface LibEditResult {
	message: string;
	change: LibEditFileChange;
	changes: LibEditFileChange[];
	diff?: string;
	first_changed_line?: number;
	warnings: string[];
}

export interface NativeLibEditApplyOutput {
	resultJson: string;
	operations: LibEditOperation[];
}

declare module "../bindings" {
	interface NativeBindings {
		libEditListMethods(): LibEditMethodInfo[];
		libEditApply(
			methodName: string,
			inputJson: string,
			files: LibEditFileSeed[],
			options?: LibEditOptions,
		): Promise<NativeLibEditApplyOutput>;
		libEditFormatHashLines(content: string, startLine?: number): string;
		libEditComputeLineHash(lineNumber: number, line: string): string;
	}
}
