//! Diff generation and replace-mode utilities.
//!
//! Provides diff string generation (numbered and unified) and the replace-mode
//! edit logic used when not in patch mode.

use similar::TextDiff;

use crate::{
	error::EditError,
	fuzzy::{self, DEFAULT_FUZZY_THRESHOLD},
	normalize::{adjust_indentation, normalize_to_lf},
};

// ─── Types ──────────────────────────────────────────────────────────────────

/// Result of a diff generation operation.
#[derive(Debug, Clone)]
pub struct DiffResult {
	/// The formatted diff string.
	pub diff:               String,
	/// 1-indexed line number of the first change in the new file, if any.
	pub first_changed_line: Option<usize>,
}

/// Options for [`replace_text`].
#[derive(Debug, Clone)]
pub struct ReplaceOptions {
	/// Allow fuzzy matching when exact match fails.
	pub fuzzy:     bool,
	/// Replace all occurrences instead of just the first.
	pub all:       bool,
	/// Similarity threshold for fuzzy matching (defaults to
	/// [`DEFAULT_FUZZY_THRESHOLD`]).
	pub threshold: Option<f64>,
}

/// Result of a [`replace_text`] operation.
#[derive(Debug, Clone)]
pub struct ReplaceResult {
	/// The content after replacements.
	pub content: String,
	/// Number of replacements made.
	pub count:   usize,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Count the number of meaningful content lines in `content`.
///
/// Splits on `\n` and drops a trailing empty element (from a final newline),
/// returning at least 1.
fn count_content_lines(content: &str) -> usize {
	let mut lines: Vec<&str> = content.split('\n').collect();
	if lines.len() > 1 && lines.last() == Some(&"") {
		lines.pop();
	}
	lines.len().max(1)
}

/// Format a single diff line with a sign prefix and right-aligned line number.
fn format_numbered_diff_line(prefix: char, line_num: usize, width: usize, content: &str) -> String {
	format!("{prefix}{line_num:>width$}|{content}")
}

// ─── Diff Generation ────────────────────────────────────────────────────────

/// Generate a diff string with line numbers and context.
///
/// Uses `similar` for line-level diffing. Context lines are shown around
/// changes; skipped regions are indicated with `...` ellipsis lines.
///
/// `context_lines` defaults to 4 in the TypeScript source.
pub fn generate_diff_string(
	old_content: &str,
	new_content: &str,
	context_lines: usize,
) -> DiffResult {
	let text_diff = TextDiff::from_lines(old_content, new_content);
	let ops = text_diff.grouped_ops(context_lines);
	let output = &mut Vec::new();

	let max_line_num = count_content_lines(old_content).max(count_content_lines(new_content));
	let width = max_line_num.to_string().len();

	// We reconstruct the numbered-diff format by walking grouped ops.
	// `grouped_ops` already handles context windowing for us, but we need
	// the custom format (not standard unified diff), so we walk the ops
	// ourselves and insert `...` separators between groups.

	let mut first_changed_line: Option<usize> = None;
	let mut prev_group_end_old: Option<usize> = None;

	for group in &ops {
		// Insert `...` separator between non-contiguous groups.
		if let Some(prev_end) = prev_group_end_old {
			let group_start_old = group.first().map(|op| op.old_range().start).unwrap_or(0);
			if group_start_old > prev_end {
				// There's a gap — show ellipsis with the line number where the gap starts.
				output.push(format_numbered_diff_line(
					' ',
					prev_end + 1, // 1-indexed
					width,
					"...",
				));
			}
		}

		for op in group {
			let tag = op.tag();
			let old_range = op.old_range();
			let new_range = op.new_range();

			match tag {
				similar::DiffTag::Equal => {
					// Context lines — print them with old line numbers.
					for (i, change) in text_diff.iter_changes(op).enumerate() {
						let line_num = old_range.start + i + 1; // 1-indexed
						let text = change.value().strip_suffix('\n').unwrap_or(change.value());
						output.push(format_numbered_diff_line(' ', line_num, width, text));
					}
				},
				similar::DiffTag::Delete => {
					if first_changed_line.is_none() {
						// First change — record the new-file line at this point.
						first_changed_line = Some(new_range.start + 1);
					}
					for (i, change) in text_diff.iter_changes(op).enumerate() {
						let line_num = old_range.start + i + 1;
						let text = change.value().strip_suffix('\n').unwrap_or(change.value());
						output.push(format_numbered_diff_line('-', line_num, width, text));
					}
				},
				similar::DiffTag::Insert => {
					if first_changed_line.is_none() {
						first_changed_line = Some(new_range.start + 1);
					}
					for (i, change) in text_diff.iter_changes(op).enumerate() {
						let line_num = new_range.start + i + 1;
						let text = change.value().strip_suffix('\n').unwrap_or(change.value());
						output.push(format_numbered_diff_line('+', line_num, width, text));
					}
				},
				similar::DiffTag::Replace => {
					if first_changed_line.is_none() {
						first_changed_line = Some(new_range.start + 1);
					}
					// Deletions first, then insertions (standard diff ordering).
					for (i, change) in text_diff.iter_changes(op).enumerate() {
						match change.tag() {
							similar::ChangeTag::Delete => {
								// The change index for deletions: within old_range.
								let line_num = old_range.start + i + 1;
								let text = change.value().strip_suffix('\n').unwrap_or(change.value());
								output.push(format_numbered_diff_line('-', line_num, width, text));
							},
							similar::ChangeTag::Insert => {
								// For insertions within a Replace, line_num is relative to
								// new_range. `iter_changes` yields deletes first, then inserts,
								// so subtract the delete count.
								let insert_idx = i - old_range.len();
								let line_num = new_range.start + insert_idx + 1;
								let text = change.value().strip_suffix('\n').unwrap_or(change.value());
								output.push(format_numbered_diff_line('+', line_num, width, text));
							},
							similar::ChangeTag::Equal => {
								// Should not appear in Replace, but handle gracefully.
								let line_num = old_range.start + i + 1;
								let text = change.value().strip_suffix('\n').unwrap_or(change.value());
								output.push(format_numbered_diff_line(' ', line_num, width, text));
							},
						}
					}
				},
			}
		}

		// Track where this group ends in old-file coordinates.
		if let Some(last_op) = group.last() {
			prev_group_end_old = Some(last_op.old_range().end);
		}
	}

	DiffResult { diff: output.join("\n"), first_changed_line }
}

/// Generate a unified diff string with `@@ -N,M +N,M @@` hunk headers.
///
/// `context_lines` defaults to 3 in the TypeScript source.
pub fn generate_unified_diff_string(
	old_content: &str,
	new_content: &str,
	context_lines: usize,
) -> DiffResult {
	let text_diff = TextDiff::from_lines(old_content, new_content);
	let mut output = Vec::new();
	let mut first_changed_line: Option<usize> = None;

	let max_line_num = count_content_lines(old_content).max(count_content_lines(new_content));
	let width = max_line_num.to_string().len();

	for hunk in text_diff
		.unified_diff()
		.context_radius(context_lines)
		.iter_hunks()
	{
		// Emit the hunk header.
		output.push(hunk.header().to_string().trim_end().to_owned());

		for change in hunk.iter_changes() {
			let line_num = match change.tag() {
				similar::ChangeTag::Delete => {
					let ln = change.old_index().map(|i| i + 1).unwrap_or(0);
					if first_changed_line.is_none() {
						first_changed_line = change.new_index().map(|i| i + 1);
					}
					ln
				},
				similar::ChangeTag::Insert => {
					let ln = change.new_index().map(|i| i + 1).unwrap_or(0);
					if first_changed_line.is_none() {
						first_changed_line = Some(ln);
					}
					ln
				},
				similar::ChangeTag::Equal => change.old_index().map(|i| i + 1).unwrap_or(0),
			};

			let prefix = match change.tag() {
				similar::ChangeTag::Delete => '-',
				similar::ChangeTag::Insert => '+',
				similar::ChangeTag::Equal => ' ',
			};

			let text = change.value().strip_suffix('\n').unwrap_or(change.value());
			output.push(format_numbered_diff_line(prefix, line_num, width, text));
		}
	}

	DiffResult { diff: output.join("\n"), first_changed_line }
}

// ─── Replace Mode ───────────────────────────────────────────────────────────

/// Find and replace text in `content`, with optional fuzzy matching.
///
/// When `options.all` is set, replaces every occurrence. Otherwise, replaces
/// exactly one — erroring if there are multiple exact matches (ambiguity).
///
/// Uses [`crate::fuzzy::find_match`] for fuzzy matching and
/// [`crate::normalize::adjust_indentation`] to preserve indentation style.
pub fn replace_text(
	content: &str,
	old_text: &str,
	new_text: &str,
	options: &ReplaceOptions,
) -> crate::Result<ReplaceResult> {
	if old_text.is_empty() {
		return Err(EditError::ValidationError { message: "oldText must not be empty.".into() });
	}

	let threshold = options.threshold.unwrap_or(DEFAULT_FUZZY_THRESHOLD);
	let mut normalized_content = normalize_to_lf(content);
	let normalized_old = normalize_to_lf(old_text);
	let normalized_new = normalize_to_lf(new_text);

	if options.all {
		return replace_all(
			&mut normalized_content,
			&normalized_old,
			&normalized_new,
			options.fuzzy,
			threshold,
		);
	}

	// ── Single replacement mode ─────────────────────────────────────────
	let outcome =
		fuzzy::find_match(&normalized_content, &normalized_old, options.fuzzy, Some(threshold));

	// Multiple exact occurrences → ambiguity error.
	if let Some(occ) = outcome.occurrences
		&& occ > 1
	{
		let lines_desc: String = outcome
			.occurrence_lines
			.iter()
			.map(|l| format!("  line {l}"))
			.collect::<Vec<_>>()
			.join("\n");
		let showing = if occ > 5 {
			format!(" (showing first 5 of {occ})")
		} else {
			String::new()
		};
		return Err(EditError::ValidationError {
			message: format!(
				"Found {occ} occurrences{showing}:\n{lines_desc}\n\nAdd more context lines to \
				 disambiguate."
			),
		});
	}

	let Some(m) = outcome.matched else {
		return Ok(ReplaceResult { content: normalized_content, count: 0 });
	};

	let adjusted = adjust_indentation(&normalized_old, &m.actual_text, &normalized_new);
	let end = m.start_index + m.actual_text.len();
	normalized_content = format!(
		"{}{}{}",
		&normalized_content[..m.start_index],
		adjusted,
		&normalized_content[end..],
	);

	Ok(ReplaceResult { content: normalized_content, count: 1 })
}

/// Replace all occurrences — exact first, then fuzzy fallback.
fn replace_all(
	content: &mut String,
	old: &str,
	new: &str,
	allow_fuzzy: bool,
	threshold: f64,
) -> crate::Result<ReplaceResult> {
	// Try exact split-join first.
	let exact_count = content.matches(old).count();
	if exact_count > 0 {
		let replaced = content.split(old).collect::<Vec<_>>().join(new);
		return Ok(ReplaceResult { content: replaced, count: exact_count });
	}

	// No exact matches — try fuzzy matching iteratively.
	let mut count = 0usize;
	loop {
		let outcome = fuzzy::find_match(content, old, allow_fuzzy, Some(threshold));

		// Accept the closest match if fuzzy is enabled, confidence is sufficient,
		// and there is at most one fuzzy candidate (no ambiguity).
		let should_use_closest = allow_fuzzy
			&& outcome
				.closest
				.as_ref()
				.is_some_and(|c| c.confidence >= threshold)
			&& outcome.fuzzy_matches.is_none_or(|n| n <= 1);

		let m = match outcome.matched.or({
			if should_use_closest {
				outcome.closest
			} else {
				None
			}
		}) {
			Some(m) => m,
			None => break,
		};

		let adjusted = adjust_indentation(old, &m.actual_text, new);
		// If the adjusted replacement is identical to what's already there,
		// stop to avoid infinite loops.
		if adjusted == m.actual_text {
			break;
		}

		let end = m.start_index + m.actual_text.len();
		*content = format!("{}{}{}", &content[..m.start_index], adjusted, &content[end..],);
		count += 1;
	}

	Ok(ReplaceResult { content: content.clone(), count })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// ── generate_diff_string ────────────────────────────────────────────

	#[test]
	fn diff_simple_change() {
		let old = "line1\nline2\nline3\nline4\n";
		let new = "line1\nline2 modified\nline3\nline4\n";
		let result = generate_diff_string(old, new, 4);

		assert!(result.diff.contains("-"), "should have deletions");
		assert!(result.diff.contains("+"), "should have insertions");
		assert!(result.diff.contains("line2"), "should reference changed line");
		assert!(result.diff.contains("line2 modified"), "should show new content");
		assert_eq!(result.first_changed_line, Some(2));
	}

	#[test]
	fn diff_no_change() {
		let content = "same\ncontent\n";
		let result = generate_diff_string(content, content, 4);
		assert!(result.diff.is_empty(), "identical content produces empty diff");
		assert_eq!(result.first_changed_line, None);
	}

	#[test]
	fn diff_insertion_only() {
		let old = "a\nb\n";
		let new = "a\nx\nb\n";
		let result = generate_diff_string(old, new, 4);
		assert!(result.diff.contains("+"), "should have insertion");
		assert!(result.diff.contains("x"), "should show inserted line");
		assert!(result.first_changed_line.is_some());
	}

	#[test]
	fn diff_deletion_only() {
		let old = "a\nb\nc\n";
		let new = "a\nc\n";
		let result = generate_diff_string(old, new, 4);
		assert!(result.diff.contains("-"), "should have deletion");
		assert!(result.diff.contains("b"), "should show deleted line");
		assert!(result.first_changed_line.is_some());
	}

	// ── generate_unified_diff_string ────────────────────────────────────

	#[test]
	fn unified_diff_has_hunk_headers() {
		let old = "line1\nline2\nline3\n";
		let new = "line1\nchanged\nline3\n";
		let result = generate_unified_diff_string(old, new, 3);
		assert!(result.diff.contains("@@"), "should have hunk header");
		assert!(result.first_changed_line.is_some());
	}

	// ── replace_text ────────────────────────────────────────────────────

	#[test]
	fn replace_exact_single() {
		let content = "hello world";
		let result = replace_text(content, "world", "rust", &ReplaceOptions {
			fuzzy:     false,
			all:       false,
			threshold: None,
		})
		.expect("should succeed");

		assert_eq!(result.content, "hello rust");
		assert_eq!(result.count, 1);
	}

	#[test]
	fn replace_exact_all() {
		let content = "aaa bbb aaa ccc aaa";
		let result = replace_text(content, "aaa", "xxx", &ReplaceOptions {
			fuzzy:     false,
			all:       true,
			threshold: None,
		})
		.expect("should succeed");

		assert_eq!(result.content, "xxx bbb xxx ccc xxx");
		assert_eq!(result.count, 3);
	}

	#[test]
	fn replace_no_match_single() {
		let content = "hello world";
		let result = replace_text(content, "missing", "new", &ReplaceOptions {
			fuzzy:     false,
			all:       false,
			threshold: None,
		})
		.expect("should succeed");

		assert_eq!(result.content, "hello world");
		assert_eq!(result.count, 0);
	}

	#[test]
	fn replace_no_match_all() {
		let content = "hello world";
		let result = replace_text(content, "missing", "new", &ReplaceOptions {
			fuzzy:     false,
			all:       true,
			threshold: None,
		})
		.expect("should succeed");

		assert_eq!(result.content, "hello world");
		assert_eq!(result.count, 0);
	}

	#[test]
	fn replace_empty_old_text_errors() {
		let result = replace_text("content", "", "new", &ReplaceOptions {
			fuzzy:     false,
			all:       false,
			threshold: None,
		});
		assert!(result.is_err(), "empty old_text should error");
		let err = result.unwrap_err();
		assert!(
			matches!(err, EditError::ValidationError { .. }),
			"should be ValidationError, got: {err:?}"
		);
	}

	#[test]
	fn replace_multiple_exact_without_all_errors() {
		let content = "foo bar foo baz foo";
		let result = replace_text(content, "foo", "xxx", &ReplaceOptions {
			fuzzy:     false,
			all:       false,
			threshold: None,
		});
		assert!(result.is_err(), "multiple occurrences without all should error");
		let err = result.unwrap_err();
		assert!(
			matches!(err, EditError::ValidationError { .. }),
			"should be ValidationError, got: {err:?}"
		);
		assert!(err.to_string().contains("3 occurrences"));
	}

	#[test]
	fn replace_normalizes_crlf() {
		let content = "hello\r\nworld\r\n";
		let result = replace_text(content, "hello\nworld", "goodbye\nworld", &ReplaceOptions {
			fuzzy:     false,
			all:       false,
			threshold: None,
		})
		.expect("should succeed with CRLF normalization");

		assert_eq!(result.count, 1);
		assert!(result.content.contains("goodbye"));
	}
}
