//! Hashline edit mode — a line-addressable edit format using text hashes.
//!
//! Each line in a file is identified by its 1-indexed line number and a short
//! hash derived from the normalized line text (CRC32, truncated to 2 chars
//! from a custom nibble alphabet).
//!
//! The combined `LINE#ID` reference acts as both an address and a staleness
//! check: if the file has changed since the caller last read it, hash
//! mismatches are caught before any mutation occurs.
//!
//! Displayed format: `LINENUM#HASH:TEXT`
//! Reference format: `"LINENUM#HASH"` (e.g. `"5#ZP"`)

use std::{
	collections::{HashMap, HashSet},
	sync::LazyLock,
};

use crc32fast::Hasher as Crc32Hasher;

use crate::{EditError, HashMismatch};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// 16-char alphabet for hash encoding (one char per nibble).
const NIBBLE_STR: &str = "ZPMQVRWSNKTXJBYH";

/// Precomputed 2-char hash lookup table: `DICT[byte]` yields a 2-char string
/// built from the high and low nibbles of `byte` mapped through [`NIBBLE_STR`].
static DICT: LazyLock<[String; 256]> = LazyLock::new(|| {
	let nibbles = NIBBLE_STR.as_bytes();
	std::array::from_fn(|i| {
		let h = (i >> 4) & 0x0f;
		let l = i & 0x0f;
		format!("{}{}", nibbles[h] as char, nibbles[l] as char)
	})
});

/// Returns `true` if `line` contains at least one Unicode letter or digit.
fn has_significant_char(line: &str) -> bool {
	line.chars().any(|c| c.is_alphanumeric())
}

// ─────────────────────────────────────────────────────────────────────────────
// Core hashing
// ─────────────────────────────────────────────────────────────────────────────

/// Compute a short hash of a single line.
///
/// Uses CRC32 on a trailing-whitespace-trimmed, CR-stripped line, truncated
/// to 2 chars from [`NIBBLE_STR`]. For lines containing no alphanumeric
/// characters (only punctuation/symbols/whitespace), the line index is mixed in
/// as the seed to reduce hash collisions.
///
/// `idx` is the 1-indexed line number. The line should not include a trailing
/// newline.
pub fn compute_line_hash(idx: usize, line: &str) -> String {
	let cleaned: String = line.replace('\r', "");
	let trimmed = cleaned.trim_end();

	let seed = if has_significant_char(trimmed) {
		0
	} else {
		idx as u32
	};

	let mut hasher = Crc32Hasher::new_with_initial(seed);
	hasher.update(trimmed.as_bytes());
	let hash_byte = (hasher.finalize() & 0xff) as usize;
	DICT[hash_byte].clone()
}

/// Format a tag for display: `"{line_num}#{hash}"`.
pub fn format_line_tag(line_num: usize, line_text: &str) -> String {
	format!("{}#{}", line_num, compute_line_hash(line_num, line_text))
}

/// Format file text with hashline prefixes for display.
///
/// Each line becomes `LINENUM#HASH:TEXT` where LINENUM is 1-indexed.
///
/// ```
/// use libedit::hashline::format_hash_lines;
/// let out = format_hash_lines("hello\nworld", 1);
/// assert!(out.contains("#"));
/// assert!(out.contains(":hello"));
/// ```
pub fn format_hash_lines(text: &str, start_line: usize) -> String {
	text
		.split('\n')
		.enumerate()
		.map(|(i, line)| {
			let num = start_line + i;
			format!("{}:{}", format_line_tag(num, line), line)
		})
		.collect::<Vec<_>>()
		.join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// Anchor types
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed line reference: 1-indexed line number + 2-char hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
	/// 1-indexed line number.
	pub line: usize,
	/// 2-char hash from the nibble alphabet.
	pub hash: String,
}

/// Parse a line reference string like `"5#XX"` into an [`Anchor`].
///
/// Accepts optional leading `>+-` and whitespace, optional surrounding spaces
/// around `#`, and optional trailing `:...` suffix. Returns
/// [`EditError::ParseError`] on failure.
pub fn parse_tag(raw: &str) -> crate::Result<Anchor> {
	try_parse_tag(raw).ok_or_else(|| EditError::ParseError {
		message:     format!(
			"Invalid line reference \"{raw}\". Expected format \"LINE#ID\" (e.g. \"5#ZP\")."
		),
		line_number: None,
	})
}

/// Like [`parse_tag`] but returns `None` instead of an error.
pub fn try_parse_tag(raw: &str) -> Option<Anchor> {
	// Strip optional leading diff markers and whitespace.
	let s = raw.trim_start();
	let s = s.trim_start_matches(['+', '-', '>']);
	let s = s.trim_start();

	// Find the `#` separator.
	let hash_pos = s.find('#')?;
	let num_part = s[..hash_pos].trim();
	let line: usize = num_part.parse().ok()?;
	if line < 1 {
		return None;
	}

	// After `#`, skip optional whitespace, then grab exactly 2 nibble-alphabet
	// chars.
	let after_hash = s[hash_pos + 1..].trim_start();
	if after_hash.len() < 2 {
		return None;
	}
	let hash_candidate = &after_hash[..2];

	// Validate both chars belong to NIBBLE_STR.
	let valid = hash_candidate.chars().all(|c| NIBBLE_STR.contains(c));
	if !valid {
		return None;
	}

	Some(Anchor { line, hash: hash_candidate.to_string() })
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation
// ─────────────────────────────────────────────────────────────────────────────

/// Validate that a line reference points to an existing line with a matching
/// hash.
///
/// Returns [`EditError::HashMismatch`] with formatted context on mismatch, or
/// [`EditError::ApplyError`] if the line is out of range.
pub fn validate_line_ref(anchor: &Anchor, file_lines: &[&str]) -> crate::Result<()> {
	if anchor.line < 1 || anchor.line > file_lines.len() {
		return Err(EditError::ApplyError {
			message: format!(
				"Line {} does not exist (file has {} lines)",
				anchor.line,
				file_lines.len()
			),
		});
	}
	let actual = compute_line_hash(anchor.line, file_lines[anchor.line - 1]);
	if actual != anchor.hash {
		let mismatches =
			vec![HashMismatch { line: anchor.line, expected: anchor.hash.clone(), actual }];
		let context = format_hash_mismatch_error(&mismatches, file_lines);
		return Err(EditError::HashMismatch { mismatches, context });
	}
	Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Hash mismatch types & formatting
// ─────────────────────────────────────────────────────────────────────────────

/// Number of context lines shown above/below each mismatched line.
const MISMATCH_CONTEXT: usize = 2;

/// Format a grep-style error message with `>>>` markers on mismatched lines.
///
/// Shows surrounding context lines so the caller can fix all stale references
/// at once.
pub fn format_hash_mismatch_error(mismatches: &[HashMismatch], file_lines: &[&str]) -> String {
	let mismatch_set: HashMap<usize, &HashMismatch> =
		mismatches.iter().map(|m| (m.line, m)).collect();

	// Collect line ranges to display.
	let mut display_lines: HashSet<usize> = HashSet::new();
	for m in mismatches {
		let lo = m.line.saturating_sub(MISMATCH_CONTEXT).max(1);
		let hi = (m.line + MISMATCH_CONTEXT).min(file_lines.len());
		for i in lo..=hi {
			display_lines.insert(i);
		}
	}

	let mut sorted: Vec<usize> = display_lines.into_iter().collect();
	sorted.sort_unstable();

	let mut lines = Vec::new();
	let plural = if mismatches.len() > 1 {
		"s have"
	} else {
		" has"
	};
	lines.push(format!(
		"{} line{plural} changed since last read. Use the updated LINE#ID references shown below \
		 (>>> marks changed lines).",
		mismatches.len()
	));
	lines.push(String::new());

	let mut prev_line: Option<usize> = None;
	for &line_num in &sorted {
		if let Some(prev) = prev_line
			&& line_num > prev + 1
		{
			lines.push("    ...".to_string());
		}
		prev_line = Some(line_num);

		let text = file_lines[line_num - 1];
		let hash = compute_line_hash(line_num, text);
		let prefix = format!("{line_num}#{hash}");

		if mismatch_set.contains_key(&line_num) {
			lines.push(format!(">>> {prefix}:{text}"));
		} else {
			lines.push(format!("    {prefix}:{text}"));
		}
	}

	lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// Edit types
// ─────────────────────────────────────────────────────────────────────────────

/// A single hashline edit operation.
#[derive(Debug, Clone)]
pub enum HashlineEdit {
	/// Replace exactly one line.
	ReplaceLine { pos: Anchor, lines: Vec<String> },
	/// Replace an inclusive range of lines.
	ReplaceRange { pos: Anchor, end: Anchor, lines: Vec<String> },
	/// Insert lines after the anchor.
	AppendAt { pos: Anchor, lines: Vec<String> },
	/// Insert lines before the anchor.
	PrependAt { pos: Anchor, lines: Vec<String> },
	/// Append lines at end of file.
	AppendFile { lines: Vec<String> },
	/// Prepend lines at start of file.
	PrependFile { lines: Vec<String> },
}

/// Result of applying hashline edits.
#[derive(Debug)]
pub struct HashlineEditResult {
	/// The resulting file text.
	pub text:               String,
	/// 1-indexed line number of the first change, if any.
	pub first_changed_line: Option<usize>,
	/// Warnings generated during application.
	pub warnings:           Vec<String>,
	/// Edits that were no-ops (content already matched).
	pub noop_edits:         Vec<NoopEdit>,
}

/// Describes an edit that was a no-op because the content already matched.
#[derive(Debug)]
pub struct NoopEdit {
	/// Index of the edit in the original edits array.
	pub edit_index: usize,
	/// Location string (e.g. `"5#ZP"` or `"EOF"`).
	pub loc:        String,
	/// Current content at that location.
	pub current:    String,
}

/// Options that influence hashline edit application behavior.
#[derive(Debug, Clone, Copy)]
pub struct HashlineApplyOptions {
	/// Whether escaped tab indentation (`\\t`) should be auto-corrected to real
	/// tabs when no real tabs are present.
	pub autocorrect_escaped_tabs: bool,
}

impl Default for HashlineApplyOptions {
	fn default() -> Self {
		Self { autocorrect_escaped_tabs: is_escaped_tab_autocorrect_enabled() }
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// Edit application
// ─────────────────────────────────────────────────────────────────────────────

/// Auto-correct escaped tab indentation (`\\t` → real tab) when no real tabs
/// are present in the edit lines.
fn autocorrect_escaped_tabs(edits: &mut [HashlineEdit], warnings: &mut Vec<String>, enabled: bool) {
	if !enabled {
		return;
	}
	for edit in edits.iter_mut() {
		let lines = match edit {
			HashlineEdit::ReplaceLine { lines, .. }
			| HashlineEdit::ReplaceRange { lines, .. }
			| HashlineEdit::AppendAt { lines, .. }
			| HashlineEdit::PrependAt { lines, .. }
			| HashlineEdit::AppendFile { lines }
			| HashlineEdit::PrependFile { lines } => lines,
		};
		if lines.is_empty() {
			continue;
		}
		let has_escaped = lines.iter().any(|l| l.contains("\\t"));
		if !has_escaped {
			continue;
		}
		let has_real = lines.iter().any(|l| l.contains('\t'));
		if has_real {
			continue;
		}
		let mut corrected_count = 0usize;
		for line in lines.iter_mut() {
			// Replace leading `\t` sequences (literal backslash-t) with real tabs.
			let trimmed = line.trim_start_matches("\\t");
			let escaped_prefix_len = line.len() - trimmed.len();
			if escaped_prefix_len > 0 {
				let tab_count = escaped_prefix_len / 2; // each `\t` is 2 chars
				corrected_count += tab_count;
				*line = format!("{}{}", "\t".repeat(tab_count), trimmed);
			}
		}
		if corrected_count == 0 {
			continue;
		}
		warnings.push(
			"Auto-corrected escaped tab indentation in edit: converted leading \\t sequence(s) to \
			 real tab characters"
				.to_string(),
		);
	}
}

fn is_escaped_tab_autocorrect_enabled() -> bool {
	match std::env::var("PI_HASHLINE_AUTOCORRECT_ESCAPED_TABS") {
		Ok(value) if value == "0" => false,
		Ok(value) if value == "1" => true,
		_ => true,
	}
}

/// Warn on suspicious `\\uDDDD` placeholders in edit content.
fn warn_suspicious_unicode(edits: &[HashlineEdit], warnings: &mut Vec<String>) {
	for edit in edits {
		let lines = match edit {
			HashlineEdit::ReplaceLine { lines, .. }
			| HashlineEdit::ReplaceRange { lines, .. }
			| HashlineEdit::AppendAt { lines, .. }
			| HashlineEdit::PrependAt { lines, .. }
			| HashlineEdit::AppendFile { lines }
			| HashlineEdit::PrependFile { lines } => lines,
		};
		if lines.is_empty() {
			continue;
		}
		let has_placeholder = lines
			.iter()
			.any(|l| l.contains("\\uDDDD") || l.contains("\\udddd"));
		if has_placeholder {
			warnings.push(
				"Detected literal \\uDDDD in edit content; no autocorrection applied. Verify whether \
				 this should be a real Unicode escape or plain text."
					.to_string(),
			);
			return; // warn once
		}
	}
}

/// Extract a mutable reference to the `lines` field of any edit variant.
fn edit_lines(edit: &HashlineEdit) -> &[String] {
	match edit {
		HashlineEdit::ReplaceLine { lines, .. }
		| HashlineEdit::ReplaceRange { lines, .. }
		| HashlineEdit::AppendAt { lines, .. }
		| HashlineEdit::PrependAt { lines, .. }
		| HashlineEdit::AppendFile { lines }
		| HashlineEdit::PrependFile { lines } => lines,
	}
}

/// Apply an array of hashline edits to file content.
///
/// Pre-validates all hash references, auto-corrects escaped tabs, warns on
/// boundary duplication, deduplicates identical edits, and applies bottom-up.
pub fn apply_hashline_edits(
	text: &str,
	edits: &[HashlineEdit],
) -> crate::Result<HashlineEditResult> {
	apply_hashline_edits_with_options(text, edits, HashlineApplyOptions::default())
}

/// Apply hashline edits with explicit options.
pub fn apply_hashline_edits_with_options(
	text: &str,
	edits: &[HashlineEdit],
	options: HashlineApplyOptions,
) -> crate::Result<HashlineEditResult> {
	if edits.is_empty() {
		return Ok(HashlineEditResult {
			text:               text.to_string(),
			first_changed_line: None,
			warnings:           Vec::new(),
			noop_edits:         Vec::new(),
		});
	}

	let file_lines_vec: Vec<String> = text.split('\n').map(String::from).collect();
	let file_lines_ref: Vec<&str> = file_lines_vec.iter().map(|s| s.as_str()).collect();
	let original_file_lines = file_lines_vec.clone();

	// Work with owned mutable copies of edits.
	let mut edits: Vec<HashlineEdit> = edits.to_vec();

	let mut first_changed_line: Option<usize> = None;
	let mut noop_edits: Vec<NoopEdit> = Vec::new();
	let mut warnings: Vec<String> = Vec::new();

	// Pre-validate: collect all hash mismatches before mutating.
	let mut mismatches: Vec<HashMismatch> = Vec::new();

	let validate_ref = |anchor: &Anchor,
	                    file_lines: &[&str],
	                    mismatches: &mut Vec<HashMismatch>|
	 -> crate::Result<bool> {
		if anchor.line < 1 || anchor.line > file_lines.len() {
			return Err(EditError::ApplyError {
				message: format!(
					"Line {} does not exist (file has {} lines)",
					anchor.line,
					file_lines.len()
				),
			});
		}
		let actual = compute_line_hash(anchor.line, file_lines[anchor.line - 1]);
		if actual == anchor.hash {
			Ok(true)
		} else {
			mismatches.push(HashMismatch { line: anchor.line, expected: anchor.hash.clone(), actual });
			Ok(false)
		}
	};

	for edit in &mut edits {
		match edit {
			HashlineEdit::ReplaceLine { pos, .. } => {
				validate_ref(pos, &file_lines_ref, &mut mismatches)?;
			},
			HashlineEdit::ReplaceRange { pos, end, .. } => {
				let start_valid = validate_ref(pos, &file_lines_ref, &mut mismatches)?;
				let end_valid = validate_ref(end, &file_lines_ref, &mut mismatches)?;
				if start_valid && end_valid && pos.line > end.line {
					return Err(EditError::ApplyError {
						message: format!(
							"Range start line {} must be <= end line {}",
							pos.line, end.line
						),
					});
				}
			},
			HashlineEdit::AppendAt { pos, lines } | HashlineEdit::PrependAt { pos, lines } => {
				validate_ref(pos, &file_lines_ref, &mut mismatches)?;
				if lines.is_empty() {
					lines.push(String::new());
				}
			},
			HashlineEdit::AppendFile { lines } | HashlineEdit::PrependFile { lines } => {
				if lines.is_empty() {
					lines.push(String::new());
				}
			},
		}
	}

	if !mismatches.is_empty() {
		return Err(EditError::HashMismatch {
			context: format_hash_mismatch_error(&mismatches, &file_lines_ref),
			mismatches,
		});
	}

	// Auto-correct and warn.
	autocorrect_escaped_tabs(&mut edits, &mut warnings, options.autocorrect_escaped_tabs);
	warn_suspicious_unicode(&edits, &mut warnings);

	// Warn on boundary duplication.
	for edit in &edits {
		let end_line = match edit {
			HashlineEdit::ReplaceLine { pos, .. } => pos.line,
			HashlineEdit::ReplaceRange { end, .. } => end.line,
			_ => continue,
		};
		let lines = edit_lines(edit);
		if lines.is_empty() {
			continue;
		}
		// 0-indexed: end_line (1-indexed) is the next line after the replaced range.
		let next_surviving_idx = end_line; // 0-indexed = end_line (since end_line is 1-indexed, index end_line is next)
		if next_surviving_idx >= original_file_lines.len() {
			continue;
		}
		let next_surviving = &original_file_lines[next_surviving_idx];
		let last_inserted = &lines[lines.len() - 1];
		let trimmed_next = next_surviving.trim();
		let trimmed_last = last_inserted.trim();
		if !trimmed_last.is_empty() && trimmed_last == trimmed_next {
			let tag = format_line_tag(end_line + 1, next_surviving);
			warnings.push(format!(
				"Possible boundary duplication: your last replacement line `{trimmed_last}` is \
				 identical to the next surviving line {tag}. If you meant to replace the entire \
				 block, set `end` to {tag} instead."
			));
		}
	}

	// Deduplicate identical edits targeting the same line(s).
	let mut seen_keys: HashMap<String, usize> = HashMap::new();
	let mut dedup_indices: HashSet<usize> = HashSet::new();
	for (i, edit) in edits.iter().enumerate() {
		let line_key = match edit {
			HashlineEdit::ReplaceLine { pos, .. } => format!("s:{}", pos.line),
			HashlineEdit::ReplaceRange { pos, end, .. } => {
				format!("r:{}:{}", pos.line, end.line)
			},
			HashlineEdit::AppendAt { pos, .. } => format!("i:{}", pos.line),
			HashlineEdit::PrependAt { pos, .. } => format!("ib:{}", pos.line),
			HashlineEdit::AppendFile { .. } => "ieof".to_string(),
			HashlineEdit::PrependFile { .. } => "ibef".to_string(),
		};
		let content = edit_lines(edit).join("\n");
		let dst_key = format!("{line_key}:{content}");
		if let std::collections::hash_map::Entry::Vacant(e) = seen_keys.entry(dst_key) {
			e.insert(i);
		} else {
			dedup_indices.insert(i);
		}
	}
	if !dedup_indices.is_empty() {
		let mut i = edits.len();
		while i > 0 {
			i -= 1;
			if dedup_indices.contains(&i) {
				edits.remove(i);
			}
		}
	}

	// Sort bottom-up: highest line first, with precedence ordering.
	struct Annotated {
		edit:       HashlineEdit,
		idx:        usize,
		sort_line:  isize,
		precedence: u8,
	}

	let num_file_lines = file_lines_vec.len();
	let annotated: Vec<Annotated> = edits
		.into_iter()
		.enumerate()
		.map(|(idx, edit)| {
			let (sort_line, precedence) = match &edit {
				HashlineEdit::ReplaceLine { pos, .. } => (pos.line as isize, 0),
				HashlineEdit::ReplaceRange { end, .. } => (end.line as isize, 0),
				HashlineEdit::AppendAt { pos, .. } => (pos.line as isize, 1),
				HashlineEdit::PrependAt { pos, .. } => (pos.line as isize, 2),
				HashlineEdit::AppendFile { .. } => (num_file_lines as isize + 1, 1),
				HashlineEdit::PrependFile { .. } => (0, 2),
			};
			Annotated { edit, idx, sort_line, precedence }
		})
		.collect();

	let mut sorted = annotated;
	sorted.sort_by(|a, b| {
		b.sort_line
			.cmp(&a.sort_line)
			.then(a.precedence.cmp(&b.precedence))
			.then(a.idx.cmp(&b.idx))
	});

	// Apply edits bottom-up.
	let mut result_lines = file_lines_vec;
	let track = |first: &mut Option<usize>, line: usize| {
		*first = Some(first.map_or(line, |f| f.min(line)));
	};

	for entry in sorted {
		let idx = entry.idx;
		match entry.edit {
			HashlineEdit::ReplaceLine { pos, lines } => {
				let orig = &original_file_lines[pos.line - 1..pos.line];
				if orig.len() == lines.len() && orig.iter().zip(lines.iter()).all(|(a, b)| a == b) {
					noop_edits.push(NoopEdit {
						edit_index: idx,
						loc:        format!("{}#{}", pos.line, pos.hash),
						current:    orig.join("\n"),
					});
				} else {
					let start = pos.line - 1;
					result_lines.splice(start..start + 1, lines);
					track(&mut first_changed_line, pos.line);
				}
			},
			HashlineEdit::ReplaceRange { pos, end, lines } => {
				let count = end.line - pos.line + 1;
				let start = pos.line - 1;
				result_lines.splice(start..start + count, lines);
				track(&mut first_changed_line, pos.line);
			},
			HashlineEdit::AppendAt { pos, lines } => {
				if lines.is_empty() {
					noop_edits.push(NoopEdit {
						edit_index: idx,
						loc:        format!("{}#{}", pos.line, pos.hash),
						current:    original_file_lines[pos.line - 1].clone(),
					});
				} else {
					let insert_at = pos.line; // after this line (0-indexed = pos.line)
					let len = lines.len();
					result_lines.splice(insert_at..insert_at, lines);
					track(&mut first_changed_line, pos.line + 1);
					let _ = len;
				}
			},
			HashlineEdit::PrependAt { pos, lines } => {
				if lines.is_empty() {
					noop_edits.push(NoopEdit {
						edit_index: idx,
						loc:        format!("{}#{}", pos.line, pos.hash),
						current:    original_file_lines[pos.line - 1].clone(),
					});
				} else {
					let insert_at = pos.line - 1;
					result_lines.splice(insert_at..insert_at, lines);
					track(&mut first_changed_line, pos.line);
				}
			},
			HashlineEdit::AppendFile { lines } => {
				if lines.is_empty() {
					noop_edits.push(NoopEdit {
						edit_index: idx,
						loc:        "EOF".to_string(),
						current:    String::new(),
					});
				} else if result_lines.len() == 1 && result_lines[0].is_empty() {
					result_lines.splice(0..1, lines);
					track(&mut first_changed_line, 1);
				} else {
					let at = result_lines.len();
					let inserted_len = lines.len();
					result_lines.splice(at..at, lines);
					track(&mut first_changed_line, at - inserted_len + 1 + inserted_len);
					// Correct: first new line is at (at + 1) in 1-indexed
					// Actually: splice at `at` means new lines start at index `at` (0-indexed)
					// 1-indexed = at + 1
					let first_new = at + 1;
					// Re-track properly
					first_changed_line =
						Some(first_changed_line.map_or(first_new, |f| f.min(first_new)));
				}
			},
			HashlineEdit::PrependFile { lines } => {
				if lines.is_empty() {
					noop_edits.push(NoopEdit {
						edit_index: idx,
						loc:        "BOF".to_string(),
						current:    String::new(),
					});
				} else if result_lines.len() == 1 && result_lines[0].is_empty() {
					result_lines.splice(0..1, lines);
				} else {
					result_lines.splice(0..0, lines);
				}
				track(&mut first_changed_line, 1);
			},
		}
	}

	Ok(HashlineEditResult {
		text: result_lines.join("\n"),
		first_changed_line,
		warnings,
		noop_edits,
	})
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// -- Hashing -----------------------------------------------------------

	#[test]
	fn hash_deterministic() {
		let h1 = compute_line_hash(1, "hello world");
		let h2 = compute_line_hash(1, "hello world");
		assert_eq!(h1, h2);
		assert_eq!(h1.len(), 2);
	}

	#[test]
	fn hash_strips_trailing_whitespace_and_cr() {
		let h1 = compute_line_hash(1, "hello");
		let h2 = compute_line_hash(1, "hello   \r");
		assert_eq!(h1, h2);
	}

	#[test]
	fn hash_empty_string() {
		let h = compute_line_hash(1, "");
		assert_eq!(h.len(), 2);
		// Empty line uses idx as seed, so different indices yield different seeds.
		// Whether the final byte differs depends on CRC32 — just verify both are valid.
		let h2 = compute_line_hash(100, "");
		// Use a distant index to make collision unlikely.
		assert_eq!(h2.len(), 2);
		// Verify the seed is actually used (different from seed=0).
		let _h_zero_seed = compute_line_hash(1, "abc"); // has significant chars → seed=0
		// h (seed=1) and h_zero_seed (seed=0) computed on different content, so no
		// assertion here. Just confirm no panic and valid output.
		assert!(NIBBLE_STR.contains(h.chars().next().expect("non-empty hash")));
	}

	#[test]
	fn hash_non_significant_uses_idx_seed() {
		// Lines with only punctuation/whitespace should use idx as seed.
		let h1 = compute_line_hash(1, "  ---  ");
		let h2 = compute_line_hash(2, "  ---  ");
		assert_ne!(h1, h2);
	}

	#[test]
	fn hash_nibble_alphabet_only() {
		let h = compute_line_hash(42, "fn main() {}");
		for c in h.chars() {
			assert!(NIBBLE_STR.contains(c), "char '{c}' not in NIBBLE_STR");
		}
	}

	// -- Formatting --------------------------------------------------------

	#[test]
	fn format_line_tag_format() {
		let tag = format_line_tag(3, "let x = 1;");
		assert!(tag.starts_with("3#"));
		assert_eq!(tag.len(), 4); // "3#XX"
	}

	#[test]
	fn format_hash_lines_basic() {
		let out = format_hash_lines("alpha\nbeta", 1);
		let lines: Vec<&str> = out.split('\n').collect();
		assert_eq!(lines.len(), 2);
		assert!(lines[0].contains(":alpha"));
		assert!(lines[1].contains(":beta"));
		assert!(lines[0].starts_with("1#"));
		assert!(lines[1].starts_with("2#"));
	}

	// -- Parsing -----------------------------------------------------------

	#[test]
	fn parse_tag_basic() {
		let anchor = parse_tag("5#ZP").expect("should parse");
		assert_eq!(anchor.line, 5);
		assert_eq!(anchor.hash, "ZP");
	}

	#[test]
	fn parse_tag_with_prefix_and_suffix() {
		let anchor = parse_tag("> 10#MQ:some content").expect("should parse");
		assert_eq!(anchor.line, 10);
		assert_eq!(anchor.hash, "MQ");
	}

	#[test]
	fn parse_tag_with_plus_prefix() {
		let anchor = parse_tag("+5#HB").expect("should parse");
		assert_eq!(anchor.line, 5);
		assert_eq!(anchor.hash, "HB");
	}

	#[test]
	fn parse_tag_with_spaces_around_hash() {
		let anchor = parse_tag("  5 # ZP").expect("should parse");
		assert_eq!(anchor.line, 5);
		assert_eq!(anchor.hash, "ZP");
	}

	#[test]
	fn parse_tag_invalid() {
		assert!(parse_tag("not_a_tag").is_err());
		assert!(parse_tag("5#aa").is_err()); // lowercase not in NIBBLE_STR
		assert!(parse_tag("").is_err());
	}

	#[test]
	fn try_parse_tag_returns_none() {
		assert!(try_parse_tag("garbage").is_none());
		assert!(try_parse_tag("5#ZP").is_some());
	}

	// -- Validation --------------------------------------------------------

	#[test]
	fn validate_line_ref_ok() {
		let line = "hello world";
		let hash = compute_line_hash(1, line);
		let anchor = Anchor { line: 1, hash };
		assert!(validate_line_ref(&anchor, &[line]).is_ok());
	}

	#[test]
	fn validate_line_ref_mismatch() {
		let anchor = Anchor { line: 1, hash: "ZZ".to_string() };
		let result = validate_line_ref(&anchor, &["hello"]);
		assert!(result.is_err());
		let err = result.unwrap_err();
		assert!(matches!(err, EditError::HashMismatch { .. }));
	}

	#[test]
	fn validate_line_ref_out_of_range() {
		let anchor = Anchor { line: 5, hash: "ZP".to_string() };
		let result = validate_line_ref(&anchor, &["only one line"]);
		assert!(result.is_err());
	}

	// -- Edit application --------------------------------------------------

	#[test]
	fn apply_empty_edits() {
		let result = apply_hashline_edits("hello\nworld", &[]).expect("should succeed");
		assert_eq!(result.text, "hello\nworld");
		assert_eq!(result.first_changed_line, None);
	}

	#[test]
	fn apply_replace_line() {
		let text = "aaa\nbbb\nccc";
		let hash = compute_line_hash(2, "bbb");
		let edits = vec![HashlineEdit::ReplaceLine {
			pos:   Anchor { line: 2, hash },
			lines: vec!["BBB".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nBBB\nccc");
		assert_eq!(result.first_changed_line, Some(2));
	}

	#[test]
	fn apply_replace_range() {
		let text = "aaa\nbbb\nccc\nddd";
		let hash2 = compute_line_hash(2, "bbb");
		let hash3 = compute_line_hash(3, "ccc");
		let edits = vec![HashlineEdit::ReplaceRange {
			pos:   Anchor { line: 2, hash: hash2 },
			end:   Anchor { line: 3, hash: hash3 },
			lines: vec!["XXX".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nXXX\nddd");
		assert_eq!(result.first_changed_line, Some(2));
	}

	#[test]
	fn apply_append_at() {
		let text = "aaa\nbbb";
		let hash1 = compute_line_hash(1, "aaa");
		let edits = vec![HashlineEdit::AppendAt {
			pos:   Anchor { line: 1, hash: hash1 },
			lines: vec!["INSERTED".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nINSERTED\nbbb");
	}

	#[test]
	fn apply_prepend_at() {
		let text = "aaa\nbbb";
		let hash2 = compute_line_hash(2, "bbb");
		let edits = vec![HashlineEdit::PrependAt {
			pos:   Anchor { line: 2, hash: hash2 },
			lines: vec!["INSERTED".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nINSERTED\nbbb");
	}

	#[test]
	fn apply_append_file_to_empty() {
		// A file with only one empty line should be replaced, not appended to.
		let text = "";
		let edits = vec![HashlineEdit::AppendFile { lines: vec!["new content".to_string()] }];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "new content");
		assert_eq!(result.first_changed_line, Some(1));
	}

	#[test]
	fn apply_prepend_file() {
		let text = "existing";
		let edits = vec![HashlineEdit::PrependFile { lines: vec!["first".to_string()] }];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "first\nexisting");
		assert_eq!(result.first_changed_line, Some(1));
	}

	#[test]
	fn apply_hash_mismatch_error() {
		let text = "hello\nworld";
		let edits = vec![HashlineEdit::ReplaceLine {
			pos:   Anchor {
				line: 1,
				hash: "ZZ".to_string(), // wrong hash
			},
			lines: vec!["goodbye".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits);
		assert!(result.is_err());
		let err = result.unwrap_err();
		assert!(matches!(err, EditError::HashMismatch { .. }));
	}

	#[test]
	fn apply_deduplicates_identical_edits() {
		let text = "aaa\nbbb\nccc";
		let hash2 = compute_line_hash(2, "bbb");
		let edits = vec![
			HashlineEdit::ReplaceLine {
				pos:   Anchor { line: 2, hash: hash2.clone() },
				lines: vec!["BBB".to_string()],
			},
			HashlineEdit::ReplaceLine {
				pos:   Anchor { line: 2, hash: hash2 },
				lines: vec!["BBB".to_string()],
			},
		];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nBBB\nccc");
	}

	#[test]
	fn apply_noop_detection() {
		let text = "aaa\nbbb\nccc";
		let hash2 = compute_line_hash(2, "bbb");
		let edits = vec![HashlineEdit::ReplaceLine {
			pos:   Anchor { line: 2, hash: hash2 },
			lines: vec!["bbb".to_string()], // same content
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "aaa\nbbb\nccc");
		assert_eq!(result.noop_edits.len(), 1);
	}

	#[test]
	fn escaped_tab_autocorrect() {
		let text = "line1";
		let hash = compute_line_hash(1, "line1");
		let edits = vec![HashlineEdit::ReplaceLine {
			pos:   Anchor { line: 1, hash },
			lines: vec!["\\t\\tindented".to_string()],
		}];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "\t\tindented");
		assert!(!result.warnings.is_empty());
	}

	#[test]
	fn escaped_tab_autocorrect_can_be_disabled() {
		let text = "line1";
		let hash = compute_line_hash(1, "line1");
		let edits = vec![HashlineEdit::ReplaceLine {
			pos:   Anchor { line: 1, hash },
			lines: vec!["\\t\\tindented".to_string()],
		}];
		let result = apply_hashline_edits_with_options(text, &edits, HashlineApplyOptions {
			autocorrect_escaped_tabs: false,
		})
		.expect("should succeed");
		assert_eq!(result.text, "\\t\\tindented");
		assert!(result.warnings.is_empty());
	}

	#[test]
	fn format_hash_mismatch_error_message() {
		let lines = vec!["aaa", "bbb", "ccc", "ddd", "eee"];
		let mismatches = vec![HashMismatch {
			line:     3,
			expected: "ZZ".to_string(),
			actual:   compute_line_hash(3, "ccc"),
		}];
		let msg = format_hash_mismatch_error(&mismatches, &lines);
		assert!(msg.contains(">>> "));
		assert!(msg.contains("1 line has changed"));
	}

	#[test]
	fn multiple_edits_sorted_bottom_up() {
		let text = "a\nb\nc\nd";
		let hash1 = compute_line_hash(1, "a");
		let hash3 = compute_line_hash(3, "c");
		let edits = vec![
			HashlineEdit::ReplaceLine {
				pos:   Anchor { line: 1, hash: hash1 },
				lines: vec!["A".to_string()],
			},
			HashlineEdit::ReplaceLine {
				pos:   Anchor { line: 3, hash: hash3 },
				lines: vec!["C".to_string()],
			},
		];
		let result = apply_hashline_edits(text, &edits).expect("should succeed");
		assert_eq!(result.text, "A\nb\nC\nd");
		assert_eq!(result.first_changed_line, Some(1));
	}
}
