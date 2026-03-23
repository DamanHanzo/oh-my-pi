//! Diff/patch parsing for the edit tool.
//!
//! Supports multiple input formats:
//! - Simple +/- diffs
//! - Unified diff format (`@@ -X,Y +A,B @@`)
//! - Codex-style wrapped patches (`*** Begin Patch` / `*** End Patch`)

use crate::error::EditError;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

/// Multi-file patch markers that indicate this is not a single-file patch.
const MULTI_FILE_MARKERS: &[&str] =
	&["*** Update File:", "*** Add File:", "*** Delete File:", "diff --git "];

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// A single hunk from a parsed diff.
#[derive(Debug, Clone)]
pub struct DiffHunk {
	/// Optional context string for locating this hunk (function name, class,
	/// etc.).
	pub change_context:    Option<String>,
	/// 1-based starting line in the old file, if known.
	pub old_start_line:    Option<usize>,
	/// 1-based starting line in the new file, if known.
	pub new_start_line:    Option<usize>,
	/// Whether this hunk contains context (unchanged) lines.
	pub has_context_lines: bool,
	/// Lines from the old file (context + removals).
	pub old_lines:         Vec<String>,
	/// Lines from the new file (context + additions).
	pub new_lines:         Vec<String>,
	/// Whether `*** End of File` was encountered at the end of this hunk.
	pub is_end_of_file:    bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal types
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed unified diff hunk header (`@@ -old,count +new,count @@ context`).
struct UnifiedHunkHeader {
	old_start_line: usize,
	#[allow(dead_code)]
	old_line_count: usize,
	new_start_line: usize,
	#[allow(dead_code)]
	new_line_count: usize,
	change_context: Option<String>,
}

/// Result of parsing a single hunk.
struct ParseHunkResult {
	hunk:           DiffHunk,
	lines_consumed: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Content-line detection
// ─────────────────────────────────────────────────────────────────────────────

/// Check if a line is a diff content line (context, addition, or removal).
///
/// Lines starting with `+++ ` or `--- ` are metadata headers, not content.
fn is_diff_content_line(line: &str) -> bool {
	match line.as_bytes().first() {
		Some(b' ') => true,
		Some(b'+') => !line.starts_with("+++ "),
		Some(b'-') => !line.starts_with("--- "),
		_ => false,
	}
}

/// Check if a line is unified diff metadata (should be stripped during
/// normalization).
fn is_unified_diff_metadata_line(line: &str) -> bool {
	line.starts_with("diff --git ")
		|| line.starts_with("index ")
		|| line.starts_with("--- ")
		|| line.starts_with("+++ ")
		|| line.starts_with("new file mode ")
		|| line.starts_with("deleted file mode ")
		|| line.starts_with("rename from ")
		|| line.starts_with("rename to ")
		|| line.starts_with("similarity index ")
		|| line.starts_with("dissimilarity index ")
		|| line.starts_with("old mode ")
		|| line.starts_with("new mode ")
}

// ─────────────────────────────────────────────────────────────────────────────
// Normalization
// ─────────────────────────────────────────────────────────────────────────────

/// Normalize a diff by stripping various wrapper formats and metadata.
///
/// Handles:
/// - `*** Begin Patch` / `*** End Patch` markers (partial or complete)
/// - Codex file markers: `*** Update File:`, `*** Add File:`, `*** Delete
///   File:`
/// - Unified diff metadata: `diff --git`, `index`, `---`, `+++`, mode changes,
///   rename markers
pub fn normalize_diff(diff: &str) -> String {
	let mut lines: Vec<&str> = diff.split('\n').collect();

	// Strip trailing truly empty lines (not diff content lines like " " which
	// represent blank context)
	while let Some(&last) = lines.last() {
		if last.is_empty() || (last.trim().is_empty() && !is_diff_content_line(last)) {
			lines.pop();
		} else {
			break;
		}
	}

	// Layer 1: Strip *** Begin Patch / *** End Patch
	if lines
		.first()
		.is_some_and(|l| l.trim().starts_with("*** Begin Patch"))
	{
		lines.remove(0);
	}
	// Strip bare *** at the beginning (model hallucination)
	if lines.first().is_some_and(|l| l.trim() == "***") {
		lines.remove(0);
	}
	if lines
		.last()
		.is_some_and(|l| l.trim().starts_with("*** End Patch"))
	{
		lines.pop();
	}
	// Strip bare *** terminator (model hallucination)
	if lines.last().is_some_and(|l| l.trim() == "***") {
		lines.pop();
	}

	// Layer 2: Strip Codex-style file operation markers and unified diff metadata.
	// NOTE: Do NOT strip "*** End of File" — it's a valid marker within hunks, not
	// a wrapper. IMPORTANT: Only strip actual metadata lines, NOT diff content
	// lines (starting with space, +, or -)
	lines.retain(|line| {
		// Preserve diff content lines even if their content looks like metadata.
		// `--- ` and `+++ ` are metadata, not content lines.
		if is_diff_content_line(line) {
			return true;
		}

		let trimmed = line.trim();

		// Codex file operation markers
		if trimmed.starts_with("*** Update File:")
			|| trimmed.starts_with("*** Add File:")
			|| trimmed.starts_with("*** Delete File:")
		{
			return false;
		}

		// Unified diff metadata
		if is_unified_diff_metadata_line(trimmed) {
			return false;
		}

		true
	});

	lines.join("\n")
}

/// Strip `+ ` prefix from file creation content if all non-empty lines have it.
///
/// This handles diffs where file content is formatted as additions.
pub fn normalize_create_content(content: &str) -> String {
	let lines: Vec<&str> = content.split('\n').collect();
	let non_empty: Vec<&&str> = lines.iter().filter(|l| !l.is_empty()).collect();

	// Check if all non-empty lines start with "+ " or "+"
	if !non_empty.is_empty()
		&& non_empty
			.iter()
			.all(|l| l.starts_with("+ ") || l.starts_with('+'))
	{
		lines
			.iter()
			.map(|l| {
				if let Some(rest) = l.strip_prefix("+ ") {
					rest
				} else if let Some(rest) = l.strip_prefix('+') {
					rest
				} else {
					l
				}
			})
			.collect::<Vec<_>>()
			.join("\n")
	} else {
		content.to_string()
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// Header parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a unified hunk header: `@@ -OLD,COUNT +NEW,COUNT @@ optional-context`
///
/// Uses manual string matching (no regex crate). Returns `None` if the line
/// doesn't match the unified header pattern.
fn parse_unified_hunk_header(line: &str) -> Option<UnifiedHunkHeader> {
	// Must start with "@@" and contain a closing "@@"
	let line = line.trim_end();
	if !line.starts_with("@@") {
		return None;
	}

	// Find the closing @@. Skip the opening "@@" (2 chars).
	let after_open = &line[2..];
	let close_pos = after_open.find("@@")?;
	let range_part = after_open[..close_pos].trim();
	let after_close = after_open[close_pos + 2..].trim();

	// range_part should look like "-N,M +N,M" or "-N +N"
	let mut parts = range_part.split_whitespace();

	let old_part = parts.next()?;
	if !old_part.starts_with('-') {
		return None;
	}
	let (old_start, old_count) = parse_range_spec(&old_part[1..])?;

	let new_part = parts.next()?;
	if !new_part.starts_with('+') {
		return None;
	}
	let (new_start, new_count) = parse_range_spec(&new_part[1..])?;

	// Anything extra between ranges is unexpected — reject
	if parts.next().is_some() {
		return None;
	}

	let change_context = if after_close.is_empty() {
		None
	} else {
		Some(after_close.to_string())
	};

	Some(UnifiedHunkHeader {
		old_start_line: old_start,
		old_line_count: old_count,
		new_start_line: new_start,
		new_line_count: new_count,
		change_context,
	})
}

/// Parse "N" or "N,M" into (start, count). Count defaults to 1 when absent.
fn parse_range_spec(s: &str) -> Option<(usize, usize)> {
	if let Some((start_s, count_s)) = s.split_once(',') {
		let start = start_s.parse::<usize>().ok()?;
		let count = count_s.parse::<usize>().ok()?;
		Some((start, count))
	} else {
		let start = s.parse::<usize>().ok()?;
		Some((start, 1))
	}
}

/// Check if a string matches the `line N` / `lines N` / `lines N-M` hint
/// pattern (case-insensitive). Returns the line number on match.
fn parse_line_hint(s: &str) -> Option<usize> {
	let lower = s.to_ascii_lowercase();
	let rest = lower
		.strip_prefix("lines ")
		.or_else(|| lower.strip_prefix("line "))?;
	// Strip optional trailing @@
	let rest = rest.strip_suffix("@@").unwrap_or(rest).trim();
	// May be "N" or "N-M"; we only need the first number
	let num_str = if let Some((n, _)) = rest.split_once('-') {
		n.trim()
	} else {
		rest
	};
	num_str.parse::<usize>().ok()
}

/// Check if a string matches "top/start/beginning of file" (case-insensitive).
fn is_top_of_file_hint(s: &str) -> bool {
	let lower = s.to_ascii_lowercase();
	lower == "top of file" || lower == "start of file" || lower == "beginning of file"
}

/// Check if a line is an empty context marker (`@@` possibly with whitespace
/// around a closing `@@`).
fn is_empty_context_marker(line: &str) -> bool {
	let trimmed = line.trim_end();
	if trimmed == EMPTY_CHANGE_CONTEXT_MARKER {
		return true;
	}
	// Match `@@  @@` pattern
	if trimmed.starts_with("@@") && trimmed.ends_with("@@") {
		let inner = &trimmed[2..trimmed.len() - 2];
		return inner.trim().is_empty();
	}
	false
}

// ─────────────────────────────────────────────────────────────────────────────
// Hunk parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a single hunk from `lines` starting at position 0.
///
/// `line_number` is the 1-based line number in the original diff (for error
/// messages). `allow_missing_context` allows the hunk to start without an `@@`
/// header.
fn parse_one_hunk(
	lines: &[&str],
	line_number: usize,
	allow_missing_context: bool,
) -> crate::Result<ParseHunkResult> {
	if lines.is_empty() {
		return Err(EditError::ParseError {
			message:     "Diff does not contain any lines".into(),
			line_number: Some(line_number),
		});
	}

	let mut change_contexts: Vec<String> = Vec::new();
	let mut old_start_line: Option<usize> = None;
	let mut new_start_line: Option<usize> = None;
	let start_index: usize;

	let header_line = lines[0];
	let header_trimmed = header_line.trim_end();
	let is_header_line = header_line.starts_with("@@");
	let unified_header = if is_header_line {
		parse_unified_hunk_header(header_trimmed)
	} else {
		None
	};

	if is_header_line
		&& (header_trimmed == EMPTY_CHANGE_CONTEXT_MARKER || is_empty_context_marker(header_trimmed))
	{
		// Bare @@ or @@ @@ — no context, match from current position
		start_index = 1;
	} else if let Some(uh) = unified_header {
		// Unified header: @@ -N,M +N,M @@ optional-context
		if uh.old_start_line < 1 || uh.new_start_line < 1 {
			return Err(EditError::ParseError {
				message:     "Line numbers in @@ header must be >= 1".into(),
				line_number: Some(line_number),
			});
		}
		if let Some(ctx) = uh.change_context {
			change_contexts.push(ctx);
		}
		old_start_line = Some(uh.old_start_line);
		new_start_line = Some(uh.new_start_line);
		start_index = 1;
	} else if is_header_line && header_trimmed.starts_with(CHANGE_CONTEXT_MARKER) {
		// Context header: @@ function foo, @@ line 125, @@ top of file, etc.
		let context_value = &header_trimmed[CHANGE_CONTEXT_MARKER.len()..];
		let trimmed_context = context_value.trim();
		// Strip leading @@ from nested context
		let normalized = if trimmed_context.starts_with("@@") {
			trimmed_context
				.strip_prefix("@@")
				.expect("just checked starts_with")
				.trim_start()
		} else {
			trimmed_context
		};

		if let Some(line_num) = parse_line_hint(normalized) {
			if line_num < 1 {
				return Err(EditError::ParseError {
					message:     "Line hint must be >= 1".into(),
					line_number: Some(line_number),
				});
			}
			old_start_line = Some(line_num);
			new_start_line = Some(line_num);
		} else if is_top_of_file_hint(normalized) {
			old_start_line = Some(1);
			new_start_line = Some(1);
		} else if !trimmed_context.is_empty() {
			change_contexts.push(context_value.to_string());
		}
		start_index = 1;
	} else if is_header_line {
		// Some other @@ line — extract context from after @@
		let context_value = header_trimmed[2..].trim();
		if !context_value.is_empty() {
			change_contexts.push(context_value.to_string());
		}
		start_index = 1;
	} else {
		// No @@ header — only allowed for the first hunk
		if !allow_missing_context {
			return Err(EditError::ParseError {
				message:     format!(
					"Expected hunk to start with @@ context marker, got: '{}'",
					lines[0]
				),
				line_number: Some(line_number),
			});
		}
		start_index = 0;
	}

	if let Some(osl) = old_start_line
		&& osl < 1
	{
		return Err(EditError::ParseError {
			message:     format!("Line numbers must be >= 1 (got {osl})"),
			line_number: Some(line_number),
		});
	}
	if let Some(nsl) = new_start_line
		&& nsl < 1
	{
		return Err(EditError::ParseError {
			message:     format!("Line numbers must be >= 1 (got {nsl})"),
			line_number: Some(line_number),
		});
	}

	// Check for nested @@ anchors on subsequent lines
	let mut si = start_index;
	while si < lines.len() {
		let next_line = lines[si];
		if !next_line.starts_with("@@") {
			break;
		}
		let trimmed = next_line.trim_end();

		if let Some(nested_context) = trimmed.strip_prefix(CHANGE_CONTEXT_MARKER) {
			if !nested_context.trim().is_empty() {
				change_contexts.push(nested_context.to_string());
			}
			si += 1;
		} else if trimmed == EMPTY_CHANGE_CONTEXT_MARKER {
			// Empty @@ as separator — skip
			si += 1;
		} else {
			// Not an @@ line we recognise, stop accumulating
			break;
		}
	}

	if si >= lines.len() {
		return Err(EditError::ParseError {
			message:     "Hunk does not contain any lines".into(),
			line_number: Some(line_number + 1),
		});
	}

	let change_context = if change_contexts.is_empty() {
		None
	} else {
		Some(change_contexts.join("\n"))
	};

	let mut hunk = DiffHunk {
		change_context,
		old_start_line,
		new_start_line,
		has_context_lines: false,
		old_lines: Vec::new(),
		new_lines: Vec::new(),
		is_end_of_file: false,
	};

	let mut parsed_lines: usize = 0;

	for i in si..lines.len() {
		let line = lines[i];
		let trimmed = line.trim();
		let next_line = lines.get(i + 1);

		// Blank line followed by @@ means next hunk
		if line.is_empty()
			&& parsed_lines > 0
			&& let Some(nl) = next_line
			&& nl.trim_start().starts_with("@@")
		{
			break;
		}

		// EOF marker
		if !is_diff_content_line(line)
			&& line.trim_end() == EOF_MARKER
			&& line.starts_with(EOF_MARKER)
		{
			if parsed_lines == 0 {
				return Err(EditError::ParseError {
					message:     "Hunk does not contain any lines".into(),
					line_number: Some(line_number + 1),
				});
			}
			hunk.is_end_of_file = true;
			parsed_lines += 1;
			break;
		}

		// Ellipsis — treat as context separator
		if trimmed == "..." || trimmed == "\u{2026}" {
			hunk.has_context_lines = true;
			parsed_lines += 1;
			continue;
		}

		let first_byte = line.as_bytes().first().copied();

		match first_byte {
			None => {
				// Empty line — treat as context
				hunk.has_context_lines = true;
				hunk.old_lines.push(String::new());
				hunk.new_lines.push(String::new());
			},
			Some(b' ') => {
				// Context line
				hunk.has_context_lines = true;
				hunk.old_lines.push(line[1..].to_string());
				hunk.new_lines.push(line[1..].to_string());
			},
			Some(b'+') => {
				// Added line
				hunk.new_lines.push(line[1..].to_string());
			},
			Some(b'-') => {
				// Removed line
				hunk.old_lines.push(line[1..].to_string());
			},
			_ => {
				if !line.starts_with("@@") {
					// Implicit context line (model omitted leading space)
					hunk.has_context_lines = true;
					hunk.old_lines.push(line.to_string());
					hunk.new_lines.push(line.to_string());
				} else {
					if parsed_lines == 0 {
						return Err(EditError::ParseError {
							message:     format!(
								"Unexpected line in hunk: '{}'. Lines must start with ' ' (context), '+' \
								 (add), or '-' (remove)",
								line
							),
							line_number: Some(line_number + 1),
						});
					}
					// Assume start of next hunk
					break;
				}
			},
		}
		parsed_lines += 1;
	}

	if parsed_lines == 0 {
		return Err(EditError::ParseError {
			message:     "Hunk does not contain any lines".into(),
			line_number: Some(line_number + si),
		});
	}

	strip_line_number_prefixes(&mut hunk);

	Ok(ParseHunkResult { hunk, lines_consumed: parsed_lines + si })
}

/// Strip spurious line-number prefixes from hunk lines.
///
/// Some models prepend line numbers like `  42  actual code`. If ≥60% of
/// non-empty lines match `\s*\d{1,6}\s+content` and the numbers are roughly
/// sequential, strip the prefix from every line.
fn strip_line_number_prefixes(hunk: &mut DiffHunk) {
	let all_lines: Vec<&str> = hunk
		.old_lines
		.iter()
		.chain(hunk.new_lines.iter())
		.map(String::as_str)
		.filter(|l| !l.trim().is_empty())
		.collect();

	if all_lines.len() < 2 {
		return;
	}

	// Collect (line_number, rest) for lines matching the pattern
	let mut number_matches: Vec<(usize, &str)> = Vec::new();
	for line in &all_lines {
		if let Some(parsed) = parse_line_number_prefix(line) {
			number_matches.push(parsed);
		}
	}

	let threshold = 2.max((all_lines.len() as f64 * 0.6).ceil() as usize);
	if number_matches.len() < threshold {
		return;
	}

	// Check sequentiality
	let numbers: Vec<usize> = number_matches.iter().map(|(n, _)| *n).collect();
	let mut sequential = 0usize;
	for i in 1..numbers.len() {
		if numbers[i] == numbers[i - 1] + 1 {
			sequential += 1;
		}
	}

	if numbers.len() >= 3 && sequential < 1.max(numbers.len() - 2) {
		return;
	}

	// Strip prefixes
	let strip = |line: &str| -> String {
		parse_line_number_prefix(line)
			.map(|(_, rest)| rest.to_string())
			.unwrap_or_else(|| line.to_string())
	};

	hunk.old_lines = hunk.old_lines.iter().map(|l| strip(l)).collect();
	hunk.new_lines = hunk.new_lines.iter().map(|l| strip(l)).collect();
}

/// Try to parse `\s*\d{1,6}\s+rest` from a line, returning `(number, rest)`.
fn parse_line_number_prefix(line: &str) -> Option<(usize, &str)> {
	let trimmed = line.trim_start();
	// Find the end of the digit sequence (1-6 digits)
	let digit_end = trimmed
		.char_indices()
		.take_while(|(i, c)| *i < 6 && c.is_ascii_digit())
		.last()
		.map(|(i, _)| i + 1)?;

	if digit_end == 0 {
		return None;
	}

	let num_str = &trimmed[..digit_end];
	let after_num = &trimmed[digit_end..];

	// Must have at least one whitespace after the number, then content
	if !after_num.starts_with(|c: char| c.is_ascii_whitespace()) {
		return None;
	}

	let rest = after_num.trim_start();
	if rest.is_empty() {
		return None;
	}

	let n = num_str.parse::<usize>().ok()?;
	Some((n, rest))
}

// ─────────────────────────────────────────────────────────────────────────────
// Multi-file marker detection
// ─────────────────────────────────────────────────────────────────────────────

/// Count multi-file markers in a diff. Returns the number of distinct file
/// paths found, or the max count of any single marker type if paths can't be
/// extracted.
fn count_multi_file_markers(diff: &str) -> usize {
	use std::collections::{HashMap, HashSet};

	let mut counts: HashMap<&str, usize> = HashMap::new();
	let mut paths: HashSet<String> = HashSet::new();

	for line in diff.split('\n') {
		if is_diff_content_line(line) {
			continue;
		}
		let trimmed = line.trim();
		for &marker in MULTI_FILE_MARKERS {
			if trimmed.starts_with(marker) {
				if let Some(path) = extract_marker_path(trimmed) {
					paths.insert(path);
				}
				*counts.entry(marker).or_insert(0) += 1;
				break;
			}
		}
	}

	if !paths.is_empty() {
		return paths.len();
	}

	counts.values().copied().max().unwrap_or(0)
}

/// Extract a file path from a multi-file marker line.
fn extract_marker_path(line: &str) -> Option<String> {
	if let Some(rest) = line.strip_prefix("diff --git ") {
		let parts: Vec<&str> = rest.split_whitespace().collect();
		let candidate = parts.get(1).or_else(|| parts.first())?;
		let stripped = candidate
			.strip_prefix("a/")
			.or_else(|| candidate.strip_prefix("b/"))
			.unwrap_or(candidate);
		return Some(stripped.to_string());
	}
	if let Some(rest) = line.strip_prefix("*** Update File:") {
		return Some(rest.trim().to_string());
	}
	if let Some(rest) = line.strip_prefix("*** Add File:") {
		return Some(rest.trim().to_string());
	}
	if let Some(rest) = line.strip_prefix("*** Delete File:") {
		return Some(rest.trim().to_string());
	}
	None
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Parse all diff hunks from a diff string.
///
/// Returns `EditError::ParseError` on malformed input. Returns an empty `Vec`
/// for an empty diff. Rejects multi-file patches (more than one file marker).
pub fn parse_hunks(diff: &str) -> crate::Result<Vec<DiffHunk>> {
	let multi_file_count = count_multi_file_markers(diff);
	if multi_file_count > 1 {
		return Err(EditError::ApplyError {
			message: format!(
				"Diff contains {} file markers. Single-file patches cannot contain multi-file markers.",
				multi_file_count,
			),
		});
	}

	let normalized = normalize_diff(diff);
	let lines: Vec<&str> = normalized.split('\n').collect();
	let mut hunks: Vec<DiffHunk> = Vec::new();
	let mut i = 0;

	while i < lines.len() {
		let line = lines[i];
		let trimmed = line.trim();

		// Skip blank lines between hunks
		if trimmed.is_empty() {
			i += 1;
			continue;
		}

		// Skip unified diff metadata lines that survived normalization,
		// but only if they're not diff content lines
		let first_byte = line.as_bytes().first().copied();
		let is_diff_content =
			first_byte == Some(b' ') || first_byte == Some(b'+') || first_byte == Some(b'-');
		if !is_diff_content && is_unified_diff_metadata_line(trimmed) {
			i += 1;
			continue;
		}

		// Lone @@ header followed by only blank lines — end of diff
		if trimmed.starts_with("@@") && lines[i + 1..].iter().all(|l| l.trim().is_empty()) {
			break;
		}

		let result = parse_one_hunk(&lines[i..], i + 1, true)?;
		hunks.push(result.hunk);
		i += result.lines_consumed;
	}

	Ok(hunks)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// ── normalize_diff ──────────────────────────────────────────────────

	#[test]
	fn normalize_diff_strips_begin_end_patch() {
		let input = "*** Begin Patch\n@@ -1,2 +1,2 @@\n-old\n+new\n*** End Patch";
		let result = normalize_diff(input);
		assert_eq!(result, "@@ -1,2 +1,2 @@\n-old\n+new");
	}

	#[test]
	fn normalize_diff_strips_unified_metadata() {
		let input = "diff --git a/foo.rs b/foo.rs\nindex abc123..def456 100644\n--- a/foo.rs\n+++ \
		             b/foo.rs\n@@ -1,3 +1,3 @@\n context\n-old\n+new";
		let result = normalize_diff(input);
		assert_eq!(result, "@@ -1,3 +1,3 @@\n context\n-old\n+new");
	}

	#[test]
	fn normalize_diff_strips_codex_markers() {
		let input = "*** Update File: src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new";
		let result = normalize_diff(input);
		assert_eq!(result, "@@ -1,1 +1,1 @@\n-old\n+new");
	}

	#[test]
	fn normalize_diff_preserves_content_lines() {
		// Lines starting with + or - that aren't +++ or --- should be preserved
		let input = "+added\n-removed\n context";
		let result = normalize_diff(input);
		assert_eq!(result, "+added\n-removed\n context");
	}

	#[test]
	fn normalize_diff_strips_trailing_blank_lines() {
		let input = "-old\n+new\n\n\n";
		let result = normalize_diff(input);
		assert_eq!(result, "-old\n+new");
	}

	#[test]
	fn normalize_diff_preserves_context_space_lines() {
		// A line that is just " " is a context line for an empty line — must survive
		let input = " \n-old\n+new";
		let result = normalize_diff(input);
		assert_eq!(result, " \n-old\n+new");
	}

	// ── normalize_create_content ────────────────────────────────────────

	#[test]
	fn normalize_create_strips_plus_prefix() {
		let input = "+ line one\n+ line two\n\n+ line four";
		let result = normalize_create_content(input);
		assert_eq!(result, "line one\nline two\n\nline four");
	}

	#[test]
	fn normalize_create_strips_bare_plus() {
		let input = "+line one\n+line two";
		let result = normalize_create_content(input);
		assert_eq!(result, "line one\nline two");
	}

	#[test]
	fn normalize_create_leaves_mixed_content() {
		let input = "+ added\nnormal line";
		let result = normalize_create_content(input);
		assert_eq!(result, "+ added\nnormal line");
	}

	// ── parse_unified_hunk_header ───────────────────────────────────────

	#[test]
	fn parse_header_standard() {
		let h = parse_unified_hunk_header("@@ -10,3 +20,5 @@").expect("should parse standard header");
		assert_eq!(h.old_start_line, 10);
		assert_eq!(h.old_line_count, 3);
		assert_eq!(h.new_start_line, 20);
		assert_eq!(h.new_line_count, 5);
		assert!(h.change_context.is_none());
	}

	#[test]
	fn parse_header_with_context() {
		let h = parse_unified_hunk_header("@@ -1,2 +1,3 @@ fn main()")
			.expect("should parse header with context");
		assert_eq!(h.change_context.as_deref(), Some("fn main()"));
	}

	#[test]
	fn parse_header_no_count() {
		let h = parse_unified_hunk_header("@@ -5 +5 @@").expect("should parse header without counts");
		assert_eq!(h.old_start_line, 5);
		assert_eq!(h.old_line_count, 1);
		assert_eq!(h.new_start_line, 5);
		assert_eq!(h.new_line_count, 1);
	}

	#[test]
	fn parse_header_rejects_non_header() {
		assert!(parse_unified_hunk_header("not a header").is_none());
		assert!(parse_unified_hunk_header("@@ function foo").is_none());
	}

	// ── parse_hunks ─────────────────────────────────────────────────────

	#[test]
	fn parse_hunks_empty_diff() {
		let hunks = parse_hunks("").expect("empty diff should succeed");
		assert!(hunks.is_empty());
	}

	#[test]
	fn parse_hunks_simple_unified() {
		let diff = "@@ -1,3 +1,3 @@\n context\n-old line\n+new line\n context2";
		let hunks = parse_hunks(diff).expect("should parse simple unified diff");
		assert_eq!(hunks.len(), 1);
		let h = &hunks[0];
		assert_eq!(h.old_start_line, Some(1));
		assert_eq!(h.new_start_line, Some(1));
		assert!(h.has_context_lines);
		assert_eq!(h.old_lines, vec!["context", "old line", "context2"]);
		assert_eq!(h.new_lines, vec!["context", "new line", "context2"]);
		assert!(!h.is_end_of_file);
	}

	#[test]
	fn parse_hunks_no_header() {
		// First hunk is allowed without @@ header
		let diff = "-removed\n+added";
		let hunks = parse_hunks(diff).expect("should parse headerless diff");
		assert_eq!(hunks.len(), 1);
		assert_eq!(hunks[0].old_lines, vec!["removed"]);
		assert_eq!(hunks[0].new_lines, vec!["added"]);
	}

	#[test]
	fn parse_hunks_with_eof_marker() {
		let diff = "@@ -1,2 +1,1 @@\n-old\n+new\n*** End of File";
		let hunks = parse_hunks(diff).expect("should parse diff with EOF marker");
		assert_eq!(hunks.len(), 1);
		assert!(hunks[0].is_end_of_file);
	}

	#[test]
	fn parse_hunks_multiple() {
		let diff = "@@ -1,2 +1,2 @@\n-a\n+b\n\n@@ -10,2 +10,2 @@\n-c\n+d";
		let hunks = parse_hunks(diff).expect("should parse multiple hunks");
		assert_eq!(hunks.len(), 2);
		assert_eq!(hunks[0].old_start_line, Some(1));
		assert_eq!(hunks[1].old_start_line, Some(10));
	}

	#[test]
	fn parse_hunks_context_header() {
		let diff = "@@ function foo\n-old\n+new";
		let hunks = parse_hunks(diff).expect("should parse context header");
		assert_eq!(hunks.len(), 1);
		assert_eq!(hunks[0].change_context.as_deref(), Some("function foo"));
	}

	#[test]
	fn parse_hunks_nested_context() {
		let diff = "@@ class Foo\n@@   method bar\n-old\n+new";
		let hunks = parse_hunks(diff).expect("should parse nested context");
		assert_eq!(hunks.len(), 1);
		assert_eq!(hunks[0].change_context.as_deref(), Some("class Foo\n  method bar"));
	}

	#[test]
	fn parse_hunks_line_hint() {
		let diff = "@@ line 42\n-old\n+new";
		let hunks = parse_hunks(diff).expect("should parse line hint");
		assert_eq!(hunks.len(), 1);
		assert_eq!(hunks[0].old_start_line, Some(42));
		assert_eq!(hunks[0].new_start_line, Some(42));
	}

	#[test]
	fn parse_hunks_rejects_multi_file() {
		let diff = "*** Update File: a.rs\n-old\n+new\n*** Update File: b.rs\n-old2\n+new2";
		let result = parse_hunks(diff);
		assert!(result.is_err());
	}

	#[test]
	fn parse_hunks_full_unified_with_metadata() {
		let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"world\");
 }";
		let hunks = parse_hunks(diff).expect("should parse full unified diff");
		assert_eq!(hunks.len(), 1);
		assert_eq!(hunks[0].old_start_line, Some(1));
		assert_eq!(hunks[0].old_lines, vec!["fn main() {", "    println!(\"hello\");", "}"]);
		assert_eq!(hunks[0].new_lines, vec!["fn main() {", "    println!(\"world\");", "}"]);
	}

	// ── helpers ─────────────────────────────────────────────────────────

	#[test]
	fn is_diff_content_line_basics() {
		assert!(is_diff_content_line(" context"));
		assert!(is_diff_content_line("+added"));
		assert!(is_diff_content_line("-removed"));
		assert!(!is_diff_content_line("+++ b/file"));
		assert!(!is_diff_content_line("--- a/file"));
		assert!(!is_diff_content_line("plain text"));
	}

	#[test]
	fn line_hint_parsing() {
		assert_eq!(parse_line_hint("line 42"), Some(42));
		assert_eq!(parse_line_hint("lines 10-20"), Some(10));
		assert_eq!(parse_line_hint("Line 5"), Some(5));
		assert_eq!(parse_line_hint("lines 100@@"), Some(100));
		assert!(parse_line_hint("not a hint").is_none());
	}

	#[test]
	fn strip_line_number_prefixes_sequential() {
		let mut hunk = DiffHunk {
			change_context:    None,
			old_start_line:    None,
			new_start_line:    None,
			has_context_lines: false,
			old_lines:         vec!["1 first".into(), "2 second".into(), "3 third".into()],
			new_lines:         vec!["1 first".into(), "2 replaced".into(), "3 third".into()],
			is_end_of_file:    false,
		};
		strip_line_number_prefixes(&mut hunk);
		assert_eq!(hunk.old_lines, vec!["first", "second", "third"]);
		assert_eq!(hunk.new_lines, vec!["first", "replaced", "third"]);
	}
}
