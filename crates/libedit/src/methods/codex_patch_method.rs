//! Codex-style apply_patch method.

use serde_json::Value;

use crate::{
	ChangeOp, EditError, EditMethod, EditResult, FileChange, Result,
	diff::generate_unified_diff_string, fs::EditFs,
};

const PROMPT: &str = include_str!("../prompts/codex_apply_patch.md");
const SCHEMA: &str = include_str!("../schemas/codex_apply_patch.json");
const CONTENT_FORMAT_LARK: &str = include_str!("../schemas/codex_apply_patch.lark");

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexDiffHunk {
	change_context: Option<String>,
	old_lines:      Vec<String>,
	new_lines:      Vec<String>,
	is_end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexFileOp {
	Add { path: String, contents: String },
	Delete { path: String },
	Update { path: String, move_to: Option<String>, hunks: Vec<CodexDiffHunk> },
}

#[derive(Debug, Clone)]
struct AppliedCodexOp {
	change:   FileChange,
	warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct CodexReplacement {
	start_index: usize,
	old_len:     usize,
	new_lines:   Vec<String>,
}

/// Codex `apply_patch` method.
///
/// The constructor keeps the existing `(allow_fuzzy, threshold)` shape for API
/// compatibility with the other edit methods, but Codex semantics do not use a
/// similarity threshold. `allow_fuzzy` only controls whether Codex's built-in
/// whitespace and Unicode punctuation lenience is enabled when locating lines.
pub struct CodexPatchMethod {
	allow_lenient_sequence_match: bool,
}

impl CodexPatchMethod {
	pub const fn new(allow_fuzzy: bool, _threshold: f64) -> Self {
		Self { allow_lenient_sequence_match: allow_fuzzy }
	}
}

impl Default for CodexPatchMethod {
	fn default() -> Self {
		Self::new(true, 0.95)
	}
}

impl EditMethod for CodexPatchMethod {
	fn name(&self) -> &str {
		"apply_patch"
	}

	fn prompt(&self) -> &str {
		PROMPT
	}

	fn schema(&self) -> &str {
		SCHEMA
	}

	fn grammar(&self) -> Option<&str> {
		Some(CONTENT_FORMAT_LARK)
	}

	fn apply(&self, input: &Value, fs: &dyn EditFs) -> Result<EditResult> {
		let patch = extract_patch_text(input)?;
		let ops = parse_codex_patch(patch)?;
		if ops.is_empty() {
			return Err(EditError::ApplyError { message: "No files were modified.".into() });
		}

		let mut changes = Vec::with_capacity(ops.len());
		let mut warnings = Vec::new();
		for op in &ops {
			let applied = self.apply_file_op(op, fs)?;
			warnings.extend(applied.warnings);
			changes.push(applied.change);
		}

		let primary_change = changes
			.first()
			.cloned()
			.ok_or_else(|| EditError::ApplyError { message: "No files were modified.".into() })?;

		let diff_result = if changes.len() == 1 {
			match (&primary_change.old_content, &primary_change.new_content, primary_change.op) {
				(Some(old), Some(new), ChangeOp::Update) => {
					let diff = generate_unified_diff_string(old, new, 1);
					(!diff.diff.is_empty()).then_some(diff)
				},
				_ => None,
			}
		} else {
			None
		};

		Ok(EditResult {
			message: summarize_changes(&changes),
			change: primary_change,
			changes,
			diff: diff_result.as_ref().map(|diff| diff.diff.clone()),
			first_changed_line: diff_result.and_then(|diff| diff.first_changed_line),
			warnings,
		})
	}
}

impl CodexPatchMethod {
	fn apply_file_op(&self, op: &CodexFileOp, fs: &dyn EditFs) -> Result<AppliedCodexOp> {
		match op {
			CodexFileOp::Add { path, contents } => apply_add_file(path, contents, fs),
			CodexFileOp::Delete { path } => apply_delete_file(path, fs),
			CodexFileOp::Update { path, move_to, hunks } => apply_update_file(
				path,
				move_to.as_deref(),
				hunks,
				fs,
				self.allow_lenient_sequence_match,
			),
		}
	}
}

fn extract_patch_text(input: &Value) -> Result<&str> {
	match input {
		Value::String(patch) => Ok(patch),
		_ => Err(EditError::InvalidInput {
			message: "apply_patch expects the raw patch text as a JSON string; do not wrap it in an \
			          object"
				.into(),
		}),
	}
}

fn apply_add_file(path: &str, contents: &str, fs: &dyn EditFs) -> Result<AppliedCodexOp> {
	let old_content = if fs.exists(path)? {
		Some(fs.read(path)?)
	} else {
		None
	};
	fs.write(path, contents)?;

	Ok(AppliedCodexOp {
		change:   FileChange {
			op: ChangeOp::Create,
			path: path.to_string(),
			new_path: None,
			old_content,
			new_content: Some(contents.to_string()),
		},
		warnings: Vec::new(),
	})
}

fn apply_delete_file(path: &str, fs: &dyn EditFs) -> Result<AppliedCodexOp> {
	if !fs.exists(path)? {
		return Err(EditError::FileNotFound { path: path.to_string() });
	}

	let old_content = fs.read(path)?;
	fs.delete(path)?;

	Ok(AppliedCodexOp {
		change:   FileChange {
			op:          ChangeOp::Delete,
			path:        path.to_string(),
			new_path:    None,
			old_content: Some(old_content),
			new_content: None,
		},
		warnings: Vec::new(),
	})
}

fn apply_update_file(
	path: &str,
	move_to: Option<&str>,
	hunks: &[CodexDiffHunk],
	fs: &dyn EditFs,
	allow_lenient_sequence_match: bool,
) -> Result<AppliedCodexOp> {
	if !fs.exists(path)? {
		return Err(EditError::FileNotFound { path: path.to_string() });
	}

	let original = fs.read(path)?;
	let patched = derive_updated_contents(&original, path, hunks, allow_lenient_sequence_match)?;

	let destination = move_to.unwrap_or(path);
	fs.write(destination, &patched)?;
	if move_to.is_some() {
		fs.delete(path)?;
	}

	Ok(AppliedCodexOp {
		change:   FileChange {
			op:          ChangeOp::Update,
			path:        path.to_string(),
			new_path:    move_to.map(str::to_string),
			old_content: Some(original),
			new_content: Some(patched),
		},
		warnings: Vec::new(),
	})
}

fn derive_updated_contents(
	original_content: &str,
	path: &str,
	hunks: &[CodexDiffHunk],
	allow_lenient_sequence_match: bool,
) -> Result<String> {
	let mut original_lines: Vec<String> = original_content.split('\n').map(str::to_string).collect();

	if original_lines.last().is_some_and(String::is_empty) {
		original_lines.pop();
	}

	let replacements =
		compute_replacements(&original_lines, path, hunks, allow_lenient_sequence_match)?;
	let mut new_lines = apply_replacements(original_lines, &replacements);

	if !new_lines.last().is_some_and(String::is_empty) {
		new_lines.push(String::new());
	}

	Ok(new_lines.join("\n"))
}

fn compute_replacements(
	original_lines: &[String],
	path: &str,
	hunks: &[CodexDiffHunk],
	allow_lenient_sequence_match: bool,
) -> Result<Vec<CodexReplacement>> {
	let mut replacements = Vec::new();
	let mut line_index = 0usize;

	for hunk in hunks {
		if let Some(ctx_line) = &hunk.change_context {
			if let Some(idx) = seek_codex_sequence(
				original_lines,
				std::slice::from_ref(ctx_line),
				line_index,
				false,
				allow_lenient_sequence_match,
			) {
				line_index = idx + 1;
			} else {
				return Err(EditError::ApplyError {
					message: format!("Failed to find context '{ctx_line}' in {path}"),
				});
			}
		}

		if hunk.old_lines.is_empty() {
			let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
				original_lines.len().saturating_sub(1)
			} else {
				original_lines.len()
			};
			replacements.push(CodexReplacement {
				start_index: insertion_idx,
				old_len:     0,
				new_lines:   hunk.new_lines.clone(),
			});
			continue;
		}

		let mut pattern: &[String] = &hunk.old_lines;
		let mut new_slice: &[String] = &hunk.new_lines;
		let mut found = seek_codex_sequence(
			original_lines,
			pattern,
			line_index,
			hunk.is_end_of_file,
			allow_lenient_sequence_match,
		);

		if found.is_none() && pattern.last().is_some_and(String::is_empty) {
			pattern = &pattern[..pattern.len() - 1];
			if new_slice.last().is_some_and(String::is_empty) {
				new_slice = &new_slice[..new_slice.len() - 1];
			}
			found = seek_codex_sequence(
				original_lines,
				pattern,
				line_index,
				hunk.is_end_of_file,
				allow_lenient_sequence_match,
			);
		}

		if let Some(start_idx) = found {
			replacements.push(CodexReplacement {
				start_index: start_idx,
				old_len:     pattern.len(),
				new_lines:   new_slice.to_vec(),
			});
			line_index = start_idx + pattern.len();
		} else {
			return Err(EditError::ApplyError {
				message: format!(
					"Failed to find expected lines in {path}:\n{}",
					hunk.old_lines.join("\n")
				),
			});
		}
	}

	replacements.sort_by_key(|replacement| replacement.start_index);
	Ok(replacements)
}

fn apply_replacements(mut lines: Vec<String>, replacements: &[CodexReplacement]) -> Vec<String> {
	for replacement in replacements.iter().rev() {
		let end = replacement.start_index + replacement.old_len;
		lines.splice(replacement.start_index..end, replacement.new_lines.iter().cloned());
	}
	lines
}

fn seek_codex_sequence(
	lines: &[String],
	pattern: &[String],
	start: usize,
	eof: bool,
	allow_lenient_sequence_match: bool,
) -> Option<usize> {
	if pattern.is_empty() {
		return Some(start);
	}

	if pattern.len() > lines.len() {
		return None;
	}

	let search_start = if eof && lines.len() >= pattern.len() {
		lines.len() - pattern.len()
	} else {
		start
	};

	if let Some(index) = seek_sequence_by(lines, pattern, search_start, |lhs, rhs| lhs == rhs) {
		return Some(index);
	}

	if !allow_lenient_sequence_match {
		return None;
	}

	if let Some(index) =
		seek_sequence_by(lines, pattern, search_start, |lhs, rhs| lhs.trim_end() == rhs.trim_end())
	{
		return Some(index);
	}

	if let Some(index) =
		seek_sequence_by(lines, pattern, search_start, |lhs, rhs| lhs.trim() == rhs.trim())
	{
		return Some(index);
	}

	seek_sequence_by(lines, pattern, search_start, |lhs, rhs| {
		normalize_codex_line(lhs) == normalize_codex_line(rhs)
	})
}

fn seek_sequence_by<F>(
	lines: &[String],
	pattern: &[String],
	search_start: usize,
	mut matches: F,
) -> Option<usize>
where
	F: FnMut(&str, &str) -> bool,
{
	for index in search_start..=lines.len().saturating_sub(pattern.len()) {
		let mut ok = true;
		for (offset, expected) in pattern.iter().enumerate() {
			if !matches(lines[index + offset].as_str(), expected.as_str()) {
				ok = false;
				break;
			}
		}
		if ok {
			return Some(index);
		}
	}
	None
}

fn normalize_codex_line(line: &str) -> String {
	line
		.trim()
		.chars()
		.map(|ch| match ch {
			'\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
			| '\u{2212}' => '-',
			'\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
			'\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
			'\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
			| '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
			| '\u{3000}' => ' ',
			other => other,
		})
		.collect()
}

fn summarize_changes(changes: &[FileChange]) -> String {
	let mut lines = vec!["Success. Updated the following files:".to_string()];
	for op in [ChangeOp::Create, ChangeOp::Update, ChangeOp::Delete] {
		for change in changes.iter().filter(|change| change.op == op) {
			let marker = match change.op {
				ChangeOp::Create => "A",
				ChangeOp::Update => "M",
				ChangeOp::Delete => "D",
			};
			let display_path = change.new_path.as_deref().unwrap_or(&change.path);
			lines.push(format!("{marker} {display_path}"));
		}
	}
	lines.join("\n")
}

fn parse_codex_patch(input: &str) -> Result<Vec<CodexFileOp>> {
	let raw_lines: Vec<&str> = input.trim().lines().collect();
	let lines = match check_patch_boundaries_strict(&raw_lines) {
		Ok(()) => raw_lines,
		Err(original_error) => check_patch_boundaries_lenient(&raw_lines, original_error)?.to_vec(),
	};

	let mut ops = Vec::new();
	let last_line_index = lines.len().saturating_sub(1);
	let mut remaining_lines = &lines[1..last_line_index];
	let mut line_number = 2usize;

	while !remaining_lines.is_empty() {
		let (op, op_lines) = parse_one_file_op(remaining_lines, line_number)?;
		ops.push(op);
		line_number += op_lines;
		remaining_lines = &remaining_lines[op_lines..];
	}

	Ok(ops)
}

fn check_patch_boundaries_strict(lines: &[&str]) -> Result<()> {
	let (first_line, last_line) = match lines {
		[] => (None, None),
		[first] => (Some(first), Some(first)),
		[first, .., last] => (Some(first), Some(last)),
	};

	let first_line = first_line.map(|line| line.trim());
	let last_line = last_line.map(|line| line.trim());

	match (first_line, last_line) {
		(Some(first), Some(last)) if first == BEGIN_PATCH_MARKER && last == END_PATCH_MARKER => {
			Ok(())
		},
		(Some(first), _) if first != BEGIN_PATCH_MARKER => Err(EditError::ParseError {
			message:     "The first line of the patch must be '*** Begin Patch'".into(),
			line_number: None,
		}),
		_ => Err(EditError::ParseError {
			message:     "The last line of the patch must be '*** End Patch'".into(),
			line_number: None,
		}),
	}
}

fn check_patch_boundaries_lenient<'a>(
	original_lines: &'a [&'a str],
	original_error: EditError,
) -> Result<&'a [&'a str]> {
	match original_lines {
		[first, .., last]
			if (first == &"<<EOF" || first == &"<<'EOF'" || first == &"<<\"EOF\"")
				&& last.ends_with("EOF")
				&& original_lines.len() >= 4 =>
		{
			let inner_lines = &original_lines[1..original_lines.len() - 1];
			check_patch_boundaries_strict(inner_lines)?;
			Ok(inner_lines)
		},
		_ => Err(original_error),
	}
}

fn parse_one_file_op(lines: &[&str], line_number: usize) -> Result<(CodexFileOp, usize)> {
	let Some(first_line) = lines.first() else {
		return Err(EditError::ParseError {
			message:     "Patch body is empty".into(),
			line_number: Some(line_number),
		});
	};

	let header = first_line.trim();
	if let Some(path) = header.strip_prefix(ADD_FILE_MARKER) {
		let mut contents = String::new();
		let mut parsed_lines = 1;
		for add_line in &lines[1..] {
			if let Some(line_to_add) = add_line.strip_prefix('+') {
				contents.push_str(line_to_add);
				contents.push('\n');
				parsed_lines += 1;
			} else {
				break;
			}
		}

		return Ok((CodexFileOp::Add { path: path.to_string(), contents }, parsed_lines));
	}

	if let Some(path) = header.strip_prefix(DELETE_FILE_MARKER) {
		return Ok((CodexFileOp::Delete { path: path.to_string() }, 1));
	}

	if let Some(path) = header.strip_prefix(UPDATE_FILE_MARKER) {
		let mut remaining_lines = &lines[1..];
		let mut parsed_lines = 1;
		let move_to = remaining_lines
			.first()
			.and_then(|line| line.strip_prefix(MOVE_TO_MARKER))
			.map(str::to_string);

		if move_to.is_some() {
			remaining_lines = &remaining_lines[1..];
			parsed_lines += 1;
		}

		let mut hunks = Vec::new();
		while !remaining_lines.is_empty() {
			if remaining_lines[0].trim().is_empty() {
				parsed_lines += 1;
				remaining_lines = &remaining_lines[1..];
				continue;
			}

			if remaining_lines[0].starts_with("***") {
				break;
			}

			let (hunk, hunk_lines) =
				parse_update_file_chunk(remaining_lines, line_number + parsed_lines, hunks.is_empty())?;
			hunks.push(hunk);
			parsed_lines += hunk_lines;
			remaining_lines = &remaining_lines[hunk_lines..];
		}

		if hunks.is_empty() {
			return Err(EditError::ParseError {
				message:     format!("Update file hunk for path '{path}' is empty"),
				line_number: Some(line_number),
			});
		}

		return Ok((CodexFileOp::Update { path: path.to_string(), move_to, hunks }, parsed_lines));
	}

	Err(EditError::ParseError {
		message:     format!(
			"'{header}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', \
			 '*** Delete File: {{path}}', '*** Update File: {{path}}'"
		),
		line_number: Some(line_number),
	})
}

fn parse_update_file_chunk(
	lines: &[&str],
	line_number: usize,
	allow_missing_context: bool,
) -> Result<(CodexDiffHunk, usize)> {
	if lines.is_empty() {
		return Err(EditError::ParseError {
			message:     "Update hunk does not contain any lines".into(),
			line_number: Some(line_number),
		});
	}

	let (change_context, start_index) = if lines[0] == EMPTY_CHANGE_CONTEXT_MARKER {
		(None, 1)
	} else if let Some(context) = lines[0].strip_prefix(CHANGE_CONTEXT_MARKER) {
		(Some(context.to_string()), 1)
	} else if allow_missing_context {
		(None, 0)
	} else {
		return Err(EditError::ParseError {
			message:     format!(
				"Expected update hunk to start with a @@ context marker, got: '{}'",
				lines[0]
			),
			line_number: Some(line_number),
		});
	};

	if start_index >= lines.len() {
		return Err(EditError::ParseError {
			message:     "Update hunk does not contain any lines".into(),
			line_number: Some(line_number + 1),
		});
	}

	let mut hunk = CodexDiffHunk {
		change_context,
		old_lines: Vec::new(),
		new_lines: Vec::new(),
		is_end_of_file: false,
	};
	let mut parsed_lines = 0usize;

	for line in &lines[start_index..] {
		match *line {
			EOF_MARKER => {
				if parsed_lines == 0 {
					return Err(EditError::ParseError {
						message:     "Update hunk does not contain any lines".into(),
						line_number: Some(line_number + 1),
					});
				}
				hunk.is_end_of_file = true;
				parsed_lines += 1;
				break;
			},
			line_contents => match line_contents.chars().next() {
				None => {
					hunk.old_lines.push(String::new());
					hunk.new_lines.push(String::new());
				},
				Some(' ') => {
					hunk.old_lines.push(line_contents[1..].to_string());
					hunk.new_lines.push(line_contents[1..].to_string());
				},
				Some('+') => {
					hunk.new_lines.push(line_contents[1..].to_string());
				},
				Some('-') => {
					hunk.old_lines.push(line_contents[1..].to_string());
				},
				_ => {
					if parsed_lines == 0 {
						return Err(EditError::ParseError {
							message:     format!(
								"Unexpected line found in update hunk: '{line_contents}'. Every line \
								 should start with ' ' (context line), '+' (added line), or '-' (removed \
								 line)"
							),
							line_number: Some(line_number + 1),
						});
					}
					break;
				},
			},
		}
		parsed_lines += 1;
	}

	Ok((hunk, parsed_lines + start_index))
}

#[allow(dead_code)]
pub const fn codex_patch_content_format_lark() -> &'static str {
	CONTENT_FORMAT_LARK
}

#[cfg(test)]
mod tests {
	use serde_json::json;
	use tempfile::tempdir;

	use super::*;
	use crate::fs::{DiskFs, InMemoryFs};

	fn apply_patch(patch: &str, fs: &dyn EditFs) -> Result<EditResult> {
		CodexPatchMethod::default().apply(&Value::String(patch.to_string()), fs)
	}

	#[test]
	fn parses_multi_file_patch() {
		let patch = "*** Begin Patch\n*** Add File: one.txt\n+hello\n*** Delete File: old.txt\n*** \
		             Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
		let parsed = parse_codex_patch(patch).expect("patch should parse");
		assert_eq!(parsed.len(), 3);
		assert!(matches!(parsed[0], CodexFileOp::Add { .. }));
		assert!(matches!(parsed[1], CodexFileOp::Delete { .. }));
		assert!(matches!(parsed[2], CodexFileOp::Update { .. }));
	}

	#[test]
	fn applies_multiple_operations_and_reports_all_changes() {
		let fs =
			InMemoryFs::with_files([("delete.txt", "obsolete\n"), ("modify.txt", "line1\nline2\n")]);
		let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+created\n*** Delete File: \
		             delete.txt\n*** Update File: modify.txt\n@@\n-line2\n+changed\n*** End Patch";

		let result = apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(fs.read("nested/new.txt").expect("file should exist"), "created\n");
		assert!(!fs.exists("delete.txt").expect("exists should succeed"));
		assert_eq!(fs.read("modify.txt").expect("file should exist"), "line1\nchanged\n");
		assert_eq!(result.changes.len(), 3);
		assert_eq!(
			result.message,
			"Success. Updated the following files:\nA nested/new.txt\nM modify.txt\nD delete.txt"
		);
	}

	#[test]
	fn add_file_overwrites_existing_file_like_codex() {
		let fs = InMemoryFs::with_files([("duplicate.txt", "old content\n")]);
		let patch = "*** Begin Patch\n*** Add File: duplicate.txt\n+new content\n*** End Patch";

		let result = apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(fs.read("duplicate.txt").expect("file should exist"), "new content\n");
		assert_eq!(result.change.op, ChangeOp::Create);
		assert_eq!(result.change.old_content.as_deref(), Some("old content\n"));
	}

	#[test]
	fn update_appends_trailing_newline() {
		let fs = InMemoryFs::with_files([("no_newline.txt", "no newline at end")]);
		let patch = "*** Begin Patch\n*** Update File: no_newline.txt\n@@\n-no newline at \
		             end\n+first line\n+second line\n*** End Patch";

		apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(
			fs.read("no_newline.txt").expect("file should exist"),
			"first line\nsecond line\n"
		);
	}

	#[test]
	fn move_overwrites_existing_destination_like_codex() {
		let fs = InMemoryFs::with_files([
			("old/name.txt", "from\n"),
			("renamed/dir/name.txt", "existing\n"),
		]);
		let patch = "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: \
		             renamed/dir/name.txt\n@@\n-from\n+new\n*** End Patch";

		let result = apply_patch(patch, &fs).expect("patch should apply");

		assert!(!fs.exists("old/name.txt").expect("exists should succeed"));
		assert_eq!(fs.read("renamed/dir/name.txt").expect("file should exist"), "new\n");
		assert_eq!(result.change.new_path.as_deref(), Some("renamed/dir/name.txt"));
	}

	#[test]
	fn pure_rename_has_no_diff_preview() {
		let fs = InMemoryFs::with_files([("old/name.txt", "same\n")]);
		let patch = "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: \
		             renamed/name.txt\n@@\n same\n*** End Patch";

		let result = apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(result.diff, None);
		assert_eq!(result.first_changed_line, None);
		assert!(!fs.exists("old/name.txt").expect("exists should succeed"));
		assert_eq!(fs.read("renamed/name.txt").expect("file should exist"), "same\n");
	}

	#[test]
	fn supports_deletion_only_update_hunk() {
		let fs = InMemoryFs::with_files([("lines.txt", "line1\nline2\nline3\n")]);
		let patch =
			"*** Begin Patch\n*** Update File: lines.txt\n@@\n line1\n-line2\n line3\n*** End Patch";

		apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(fs.read("lines.txt").expect("file should exist"), "line1\nline3\n");
	}

	#[test]
	fn supports_end_of_file_marker() {
		let fs = InMemoryFs::with_files([("tail.txt", "first\nsecond\n")]);
		let patch = "*** Begin Patch\n*** Update File: tail.txt\n@@\n first\n-second\n+second \
		             updated\n*** End of File\n*** End Patch";

		apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(fs.read("tail.txt").expect("file should exist"), "first\nsecond updated\n");
	}

	#[test]
	fn partial_success_leaves_prior_changes() {
		let fs = InMemoryFs::new();
		let patch = "*** Begin Patch\n*** Add File: created.txt\n+hello\n*** Update File: \
		             missing.txt\n@@\n-old\n+new\n*** End Patch";

		let error = apply_patch(patch, &fs).expect_err("patch should fail");

		assert!(matches!(error, EditError::FileNotFound { .. }));
		assert_eq!(fs.read("created.txt").expect("file should exist"), "hello\n");
	}

	#[test]
	fn accepts_whitespace_padded_markers_and_heredoc_wrapper() {
		let fs = InMemoryFs::with_files([("file.txt", "one\n")]);
		let patch = "<<'EOF'\n *** Begin Patch\n  *** Update File: file.txt\n@@\n-one\n+two\n *** \
		             End Patch\nEOF\n";

		let result = apply_patch(patch, &fs).expect("patch should apply");

		assert_eq!(fs.read("file.txt").expect("file should exist"), "two\n");
		assert_eq!(result.message, "Success. Updated the following files:\nM file.txt");
	}

	#[test]
	fn rejects_object_wrapper_to_keep_contract_honest() {
		let fs = InMemoryFs::with_files([("file.txt", "one\n")]);
		let input = json!({
			 "input": "*** Begin Patch\n*** Update File: file.txt\n@@\n-one\n+two\n*** End Patch"
		});

		let error = CodexPatchMethod::default()
			.apply(&input, &fs)
			.expect_err("object wrapper should be rejected");

		assert!(matches!(error, EditError::InvalidInput { .. }));
		assert_eq!(
			error.to_string(),
			"Invalid input: apply_patch expects the raw patch text as a JSON string; do not wrap it \
			 in an object"
		);
	}

	#[test]
	fn delete_directory_fails_like_disk_apply_patch() {
		let tmp = tempdir().expect("tempdir should succeed");
		let target = tmp.path().join("dir");
		std::fs::create_dir(&target).expect("directory should be created");
		let patch = format!("*** Begin Patch\n*** Delete File: {}\n*** End Patch", target.display());

		let error = apply_patch(&patch, &DiskFs).expect_err("deleting a directory should fail");

		assert!(matches!(error, EditError::Io { .. }));
		assert!(target.exists(), "directory should remain after failure");
	}

	#[test]
	fn empty_patch_fails_at_apply_time() {
		let fs = InMemoryFs::with_files([("foo.txt", "keep\n")]);
		let patch = "*** Begin Patch\n*** End Patch";

		let error = apply_patch(patch, &fs).expect_err("empty patch should fail");

		assert!(matches!(error, EditError::ApplyError { .. }));
		assert_eq!(error.to_string(), "No files were modified.");
		assert_eq!(fs.read("foo.txt").expect("file should exist"), "keep\n");
	}
}
