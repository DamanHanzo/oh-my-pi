//! Patch application logic for the patch edit method.
//!
//! Applies parsed diff hunks to file content using fuzzy matching
//! for robust handling of whitespace and formatting differences.

use std::collections::HashMap;

use crate::{
	ChangeOp, EditError, FileChange, Result,
	fs::EditFs,
	fuzzy::{
		ContextLineResult, SequenceSearchResult, find_closest_sequence_match, find_context_line,
		find_match, seek_sequence,
	},
	normalize::{
		adjust_indentation, convert_leading_tabs_to_spaces, count_leading_whitespace,
		detect_line_ending, get_leading_whitespace, normalize_to_lf, restore_line_endings, strip_bom,
	},
	parser::{DiffHunk, normalize_create_content, parse_hunks},
};

// ─── Constants ───────────────────────────────────────────────────────────────

/// Window around a line hint within which an ambiguous match is accepted.
const AMBIGUITY_HINT_WINDOW: usize = 200;

/// Context lines shown before/after a match preview.
const MATCH_PREVIEW_CONTEXT: usize = 2;

/// Maximum line length in a match preview.
const MATCH_PREVIEW_MAX_LEN: usize = 80;

// ─── Public types ────────────────────────────────────────────────────────────

/// Supported patch operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
	/// Create a new file from full content.
	Create,
	/// Delete an existing file.
	Delete,
	/// Update an existing file with diff hunks.
	Update,
}

/// JSON-derived input for patch application.
#[derive(Debug, Clone)]
pub struct PatchInput {
	/// Source path.
	pub path:   String,
	/// Operation to perform.
	pub op:     Operation,
	/// Optional rename destination for updates.
	pub rename: Option<String>,
	/// Diff hunks for updates or full content for creates.
	pub diff:   Option<String>,
}

/// Options controlling patch application.
#[derive(Debug, Clone, Copy)]
pub struct ApplyPatchOptions {
	/// Allow fuzzy matching when locating hunks.
	pub allow_fuzzy:     bool,
	/// Threshold retained for future tuning.
	pub fuzzy_threshold: f64,
	/// Dry-run without mutating the filesystem.
	pub dry_run:         bool,
}

impl Default for ApplyPatchOptions {
	fn default() -> Self {
		Self { allow_fuzzy: true, fuzzy_threshold: 0.95, dry_run: false }
	}
}

/// Result of applying a patch.
#[derive(Debug, Clone)]
pub struct ApplyPatchResult {
	/// Concrete file change produced by the patch.
	pub change:   FileChange,
	/// Warnings emitted while applying hunks.
	pub warnings: Vec<String>,
}

// ─── Internal types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Replacement {
	start_index: usize,
	old_len:     usize,
	new_lines:   Vec<String>,
}

/// Kind of fallback transformation applied to a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HunkVariantKind {
	TrimCommon,
	DedupeShared,
	CollapseRepeated,
	SingleLine,
}

#[derive(Debug, Clone)]
struct HunkVariant {
	old_lines: Vec<String>,
	new_lines: Vec<String>,
	kind:      HunkVariantKind,
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Apply a patch operation to the provided filesystem.
pub fn apply_patch(
	input: &PatchInput,
	fs: &dyn EditFs,
	options: ApplyPatchOptions,
) -> Result<ApplyPatchResult> {
	if let Some(rename) = &input.rename
		&& rename == &input.path
	{
		return Err(EditError::SamePathRename);
	}

	match input.op {
		Operation::Create => apply_create(input, fs, options),
		Operation::Delete => apply_delete(input, fs, options),
		Operation::Update => apply_update(input, fs, options),
	}
}

/// Preview a patch without mutating the filesystem.
pub fn preview_patch(
	input: &PatchInput,
	fs: &dyn EditFs,
	mut options: ApplyPatchOptions,
) -> Result<ApplyPatchResult> {
	options.dry_run = true;
	apply_patch(input, fs, options)
}

// ─── Operation handlers ──────────────────────────────────────────────────────

fn apply_create(
	input: &PatchInput,
	fs: &dyn EditFs,
	options: ApplyPatchOptions,
) -> Result<ApplyPatchResult> {
	let diff = input
		.diff
		.as_deref()
		.ok_or_else(|| EditError::InvalidInput {
			message: "create operation requires 'diff'".into(),
		})?;
	let normalized = normalize_create_content(diff);
	let final_content = if normalized.ends_with('\n') {
		normalized
	} else {
		format!("{normalized}\n")
	};
	if !options.dry_run {
		fs.write(&input.path, &final_content)?;
	}
	Ok(ApplyPatchResult {
		change:   FileChange {
			op:          ChangeOp::Create,
			path:        input.path.clone(),
			new_path:    None,
			old_content: None,
			new_content: Some(final_content),
		},
		warnings: Vec::new(),
	})
}

fn apply_delete(
	input: &PatchInput,
	fs: &dyn EditFs,
	options: ApplyPatchOptions,
) -> Result<ApplyPatchResult> {
	if !fs.exists(&input.path)? {
		return Err(EditError::FileNotFound { path: input.path.clone() });
	}
	let old_content = fs.read(&input.path)?;
	if !options.dry_run {
		fs.delete(&input.path)?;
	}
	Ok(ApplyPatchResult {
		change:   FileChange {
			op:          ChangeOp::Delete,
			path:        input.path.clone(),
			new_path:    None,
			old_content: Some(old_content),
			new_content: None,
		},
		warnings: Vec::new(),
	})
}

fn apply_update(
	input: &PatchInput,
	fs: &dyn EditFs,
	options: ApplyPatchOptions,
) -> Result<ApplyPatchResult> {
	let diff = input
		.diff
		.as_deref()
		.ok_or_else(|| EditError::InvalidInput {
			message: "update operation requires 'diff'".into(),
		})?;
	if !fs.exists(&input.path)? {
		return Err(EditError::FileNotFound { path: input.path.clone() });
	}

	let raw = fs.read(&input.path)?;
	let bom = strip_bom(&raw);
	let line_ending = detect_line_ending(bom.text);
	let normalized = normalize_to_lf(bom.text);
	let hunks = parse_hunks(diff)?;
	if hunks.is_empty() {
		return Err(EditError::ParseError {
			message:     "Diff contains no hunks".into(),
			line_number: None,
		});
	}

	let (patched, warnings) = apply_hunks_to_content(
		&normalized,
		&input.path,
		&hunks,
		options.fuzzy_threshold,
		options.allow_fuzzy,
		false,
	)?;
	if patched == normalized && input.rename.is_none() {
		return Err(EditError::NoChanges {
			file:   input.path.clone(),
			detail: " The patch produced identical content.".into(),
		});
	}

	let final_content = format!("{}{}", bom.bom, restore_line_endings(&patched, line_ending));
	let write_path = input.rename.as_deref().unwrap_or(&input.path);
	if !options.dry_run {
		fs.write(write_path, &final_content)?;
		if input.rename.is_some() {
			fs.delete(&input.path)?;
		}
	}

	Ok(ApplyPatchResult {
		change: FileChange {
			op:          ChangeOp::Update,
			path:        input.path.clone(),
			new_path:    input.rename.clone(),
			old_content: Some(normalized),
			new_content: Some(patched),
		},
		warnings,
	})
}

// ─── Hunk application ────────────────────────────────────────────────────────

/// Apply diff hunks to file content. Returns `(new_content, warnings)`.
///
/// Detects single-hunk diffs without context and delegates to character-level
/// matching; otherwise splits into lines and computes per-hunk replacements.
pub(crate) fn apply_hunks_to_content(
	original_content: &str,
	path: &str,
	hunks: &[DiffHunk],
	fuzzy_threshold: f64,
	allow_fuzzy: bool,
	ensure_final_newline: bool,
) -> Result<(String, Vec<String>)> {
	let had_final_newline = original_content.ends_with('\n');

	// Single-hunk character-level matching: no context, no hints, no EOF marker.
	if hunks.len() == 1 {
		let hunk = &hunks[0];
		if hunk.change_context.is_none()
			&& !hunk.has_context_lines
			&& !hunk.old_lines.is_empty()
			&& hunk.old_start_line.is_none()
			&& !hunk.is_end_of_file
		{
			let (content, warnings) =
				apply_character_match(original_content, path, hunk, fuzzy_threshold, allow_fuzzy)?;
			return Ok((
				apply_trailing_newline_policy(&content, had_final_newline, ensure_final_newline),
				warnings,
			));
		}
	}

	let mut original_lines: Vec<String> = original_content.split('\n').map(str::to_string).collect();
	let mut stripped_final_empty = false;
	if had_final_newline && original_lines.last().is_some_and(|l| l.is_empty()) {
		original_lines.pop();
		stripped_final_empty = true;
	}

	let (replacements, warnings) = compute_replacements(&original_lines, path, hunks, allow_fuzzy)?;
	let mut result = apply_replacements(&original_lines, &replacements);

	if stripped_final_empty {
		result.push(String::new());
	}

	let mut text = result.join("\n");
	if ensure_final_newline {
		if !text.ends_with('\n') {
			text.push('\n');
		}
	} else {
		if had_final_newline && !text.ends_with('\n') {
			text.push('\n');
		}
		if !had_final_newline && text.ends_with('\n') {
			text.pop();
		}
	}

	Ok((text, warnings))
}

/// Apply a single hunk via character-level fuzzy matching.
///
/// Used when the hunk has no context lines, no line hints, and no EOF marker —
/// the simplest diff shape where line-based search adds no value.
fn apply_character_match(
	original_content: &str,
	path: &str,
	hunk: &DiffHunk,
	fuzzy_threshold: f64,
	allow_fuzzy: bool,
) -> Result<(String, Vec<String>)> {
	let old_text = hunk.old_lines.join("\n");
	let new_text = hunk.new_lines.join("\n");

	let normalized_content = normalize_to_lf(original_content);
	let normalized_old = normalize_to_lf(&old_text);

	let mut outcome =
		find_match(&normalized_content, &normalized_old, allow_fuzzy, Some(fuzzy_threshold));

	// Retry with relaxed threshold when the primary attempt fails.
	if outcome.matched.is_none() && allow_fuzzy {
		let relaxed = fuzzy_threshold.min(0.92);
		if relaxed < fuzzy_threshold {
			let relaxed_outcome =
				find_match(&normalized_content, &normalized_old, allow_fuzzy, Some(relaxed));
			if relaxed_outcome.matched.is_some() {
				outcome = relaxed_outcome;
			}
		}
	}

	// Multiple exact occurrences → ambiguous.
	if let Some(count) = outcome.occurrences
		&& count > 1
	{
		let previews = outcome.occurrence_previews.join("\n\n");
		let more = if count > 5 {
			format!(" (showing first 5 of {count})")
		} else {
			String::new()
		};
		return Err(EditError::AmbiguousMatch {
			file: path.to_string(),
			count,
			previews: format!(
				"Found {count} occurrences in {path}{more}:\n\n{previews}\n\nAdd more context lines \
				 to disambiguate."
			),
		});
	}

	// Multiple fuzzy matches → ambiguous.
	if let Some(fuzzy_count) = outcome.fuzzy_matches
		&& fuzzy_count > 1
	{
		return Err(EditError::AmbiguousMatch {
			file:     path.to_string(),
			count:    fuzzy_count,
			previews: format!(
				"Found {fuzzy_count} high-confidence matches in {path}. The text must be unique. \
				 Please provide more context to make it unique."
			),
		});
	}

	let matched = match outcome.matched {
		Some(m) => m,
		None => {
			if let Some(closest) = &outcome.closest {
				let sim = (closest.confidence * 100.0).round() as u32;
				return Err(EditError::NoMatch {
					file:   path.to_string(),
					detail: format!(
						"Could not find a close enough match in {path}. Closest match ({sim}% similar) \
						 at line {}.",
						closest.start_line
					),
				});
			}
			return Err(EditError::NoMatch {
				file:   path.to_string(),
				detail: format!("Failed to find expected lines in {path}:\n{old_text}"),
			});
		},
	};

	let adjusted = adjust_indentation(&normalized_old, &matched.actual_text, &new_text);

	let mut warnings = Vec::new();
	if outcome.dominant_fuzzy {
		let sim = (matched.confidence * 100.0).round() as u32;
		warnings.push(format!(
			"Dominant fuzzy match selected in {path} near line {} ({sim}% similar).",
			matched.start_line
		));
	}

	let before = &normalized_content[..matched.start_index];
	let after = &normalized_content[matched.start_index + matched.actual_text.len()..];
	Ok((format!("{before}{adjusted}{after}"), warnings))
}

/// Preserve or strip trailing newline to match the original file's policy.
fn apply_trailing_newline_policy(
	content: &str,
	had_final_newline: bool,
	ensure_final_newline: bool,
) -> String {
	if ensure_final_newline {
		if content.ends_with('\n') {
			content.to_owned()
		} else {
			format!("{content}\n")
		}
	} else if had_final_newline {
		if content.ends_with('\n') {
			content.to_owned()
		} else {
			format!("{content}\n")
		}
	} else {
		content.trim_end_matches('\n').to_owned()
	}
}

/// Apply replacements in reverse order so earlier indices stay valid.
fn apply_replacements(lines: &[String], replacements: &[Replacement]) -> Vec<String> {
	let mut result: Vec<String> = lines.to_vec();
	for rep in replacements.iter().rev() {
		let end = rep.start_index + rep.old_len;
		result.splice(rep.start_index..end, rep.new_lines.iter().cloned());
	}
	result
}

// ─── Replacement computation ─────────────────────────────────────────────────

/// Compute the set of replacements needed to transform `original_lines`
/// according to `hunks`. Each hunk is located via context, line hints, and
/// fuzzy matching.
fn compute_replacements(
	original_lines: &[String],
	path: &str,
	hunks: &[DiffHunk],
	allow_fuzzy: bool,
) -> Result<(Vec<Replacement>, Vec<String>)> {
	let mut replacements: Vec<Replacement> = Vec::new();
	let mut warnings: Vec<String> = Vec::new();
	let mut line_index: usize = 0;
	let line_refs: Vec<&str> = original_lines.iter().map(String::as_str).collect();

	for hunk in hunks {
		// Validate line hints.
		if let Some(hint) = hunk.old_start_line
			&& hint < 1
		{
			return Err(EditError::InvalidRange {
				message: format!(
					"Line hint {hint} is out of range for {path} (line numbers start at 1)"
				),
			});
		}
		if let Some(hint) = hunk.new_start_line
			&& hint < 1
		{
			return Err(EditError::InvalidRange {
				message: format!(
					"Line hint {hint} is out of range for {path} (line numbers start at 1)"
				),
			});
		}

		let line_hint = hunk.old_start_line;
		let allow_aggressive =
			hunk.change_context.is_some() || line_hint.is_some() || hunk.is_end_of_file;

		// Advance line_index from a bare line hint (no context, no context lines).
		if line_hint.is_some() && hunk.change_context.is_none() && !hunk.has_context_lines {
			let hint = line_hint.expect("checked is_some");
			line_index = hint
				.saturating_sub(1)
				.min(original_lines.len().saturating_sub(1));
		}

		let mut context_index: Option<usize> = None;

		// ── Locate via change_context ────────────────────────────────────
		if let Some(context) = &hunk.change_context {
			let result =
				find_hierarchical_context(&line_refs, context, line_index, line_hint, allow_fuzzy);
			context_index = result.index;

			if result.index.is_none() || matches!(result.match_count, Some(c) if c > 1) {
				// Try direct sequence fallback before giving up.
				let fallback = attempt_sequence_fallback(
					&line_refs,
					hunk,
					line_index,
					line_hint,
					allow_fuzzy,
					allow_aggressive,
				);
				if let Some(fb_idx) = fallback {
					line_index = fb_idx;
				} else if matches!(result.match_count, Some(c) if c > 1) {
					let display_context = if context.contains('\n') {
						context.split('\n').next_back().unwrap_or(context)
					} else {
						context.as_str()
					};
					let previews_text = format_sequence_match_previews(
						&line_refs,
						&result.match_indices,
						result.match_count,
					);
					let strategy_hint = match &result.strategy {
						Some(s) => format!(" Matching strategy: {s:?}."),
						None => String::new(),
					};
					let preview_block = if let Some(p) = &previews_text {
						format!("\n\n{p}")
					} else {
						String::new()
					};
					return Err(EditError::AmbiguousMatch {
						file:     path.to_string(),
						count:    result.match_count.unwrap_or(2),
						previews: format!(
							"Found {} matches for context '{display_context}' in \
							 {path}.{strategy_hint}{preview_block}\n\nAdd more surrounding context or \
							 additional @@ anchors to make it unique.",
							result.match_count.unwrap_or(2),
						),
					});
				} else {
					let display_context = if context.contains('\n') {
						context.replace('\n', " > ")
					} else {
						context.clone()
					};
					return Err(EditError::NoMatch {
						file:   path.to_string(),
						detail: format!("Failed to find context '{display_context}' in {path}"),
					});
				}
			} else if let Some(idx) = result.index {
				// If old_lines[0] matches the final context, start at idx (not idx+1).
				let first_old = hunk.old_lines.first().map(String::as_str);
				let final_context = if context.contains('\n') {
					context.split('\n').next_back().map(str::trim)
				} else {
					Some(context.trim())
				};
				let is_hierarchical = context.contains('\n') || context.split_whitespace().count() > 2;

				if first_old.is_some() && (first_old.map(str::trim) == final_context || is_hierarchical)
				{
					line_index = idx;
				} else {
					line_index = idx + 1;
				}
			}
		}

		// ── Pure addition (no old_lines) ─────────────────────────────────
		if hunk.old_lines.is_empty() {
			let insertion_idx = if hunk.change_context.is_some() {
				// Context was processed above; line_index is set.
				line_index
			} else {
				let hint_for_insert = hunk.old_start_line.or(hunk.new_start_line);
				if let Some(hint) = hint_for_insert {
					if hint < 1 {
						return Err(EditError::InvalidRange {
							message: format!(
								"Line hint {hint} is out of range for insertion in {path} (line numbers \
								 start at 1)"
							),
						});
					}
					if hint > original_lines.len() + 1 {
						return Err(EditError::InvalidRange {
							message: format!(
								"Line hint {hint} is out of range for insertion in {path} (file has {} \
								 lines)",
								original_lines.len()
							),
						});
					}
					hint.saturating_sub(1)
				} else if !original_lines.is_empty()
					&& original_lines.last().is_some_and(|l| l.is_empty())
				{
					original_lines.len() - 1
				} else {
					original_lines.len()
				}
			};

			replacements.push(Replacement {
				start_index: insertion_idx,
				old_len:     0,
				new_lines:   hunk.new_lines.clone(),
			});
			continue;
		}

		// ── Find old lines in the file ───────────────────────────────────
		let mut pattern: Vec<String> = hunk.old_lines.clone();
		let match_hint = get_hunk_hint_index(hunk, line_index);
		let mut search = find_sequence_with_hint(
			&line_refs,
			&to_refs(&pattern),
			line_index,
			match_hint,
			hunk.is_end_of_file,
			allow_fuzzy,
		);
		let mut new_slice: Vec<String> = hunk.new_lines.clone();

		// Retry without trailing empty line if present.
		if search.index.is_none() && pattern.last().is_some_and(|l| l.is_empty()) {
			pattern.pop();
			if new_slice.last().is_some_and(|l| l.is_empty()) {
				new_slice.pop();
			}
			search = find_sequence_with_hint(
				&line_refs,
				&to_refs(&pattern),
				line_index,
				match_hint,
				hunk.is_end_of_file,
				allow_fuzzy,
			);
		}

		// Try fallback variants when primary search fails or is ambiguous.
		if search.index.is_none() || matches!(search.match_count, Some(c) if c > 1) {
			for variant in filter_fallback_variants(&build_fallback_variants(hunk), allow_aggressive) {
				if variant.old_lines.is_empty() {
					continue;
				}
				let vr = find_sequence_with_hint(
					&line_refs,
					&to_refs(&variant.old_lines),
					line_index,
					match_hint,
					hunk.is_end_of_file,
					allow_fuzzy,
				);
				if vr.index.is_some() && matches!(vr.match_count, None | Some(0 | 1)) {
					pattern = variant.old_lines.clone();
					new_slice = variant.new_lines.clone();
					search = vr;
					break;
				}
			}
		}

		// Context-relative single-line fallback.
		if search.index.is_none()
			&& let Some(ctx_idx) = context_index
		{
			for variant in filter_fallback_variants(&build_fallback_variants(hunk), allow_aggressive) {
				if variant.old_lines.len() != 1 || variant.new_lines.len() != 1 {
					continue;
				}
				let removed = &variant.old_lines[0];
				let has_shared_dup = hunk.new_lines.iter().any(|l| l.trim() == removed.trim());
				let adj = find_context_relative_match(&line_refs, removed, ctx_idx, has_shared_dup);
				if let Some(adj_idx) = adj {
					pattern = variant.old_lines.clone();
					new_slice = variant.new_lines.clone();
					search = SequenceSearchResult {
						index:         Some(adj_idx),
						confidence:    0.95,
						match_count:   None,
						match_indices: vec![],
						strategy:      None,
					};
					break;
				}
			}
		}

		// Disambiguate single-line match near context.
		if search.index.is_some() && context_index.is_some() && pattern.len() == 1 {
			let trimmed = pattern[0].trim();
			let occurrence_count = line_refs.iter().filter(|l| l.trim() == trimmed).count();
			if occurrence_count > 1 {
				let has_shared_dup = hunk.new_lines.iter().any(|l| l.trim() == trimmed);
				let ctx_idx = context_index.expect("checked is_some");
				if let Some(cm) =
					find_context_relative_match(&line_refs, &pattern[0], ctx_idx, has_shared_dup)
				{
					search = SequenceSearchResult {
						index:         Some(cm),
						confidence:    search.confidence,
						match_count:   None,
						match_indices: vec![],
						strategy:      search.strategy,
					};
				}
			}
		}

		// Disambiguate via hint window.
		if matches!(search.match_count, Some(c) if c > 1) {
			let hint_idx = match_hint.or(line_hint.map(|h| h.saturating_sub(1)));
			if let Some(chosen) =
				choose_hinted_match(&search.match_indices, hint_idx, AMBIGUITY_HINT_WINDOW)
			{
				search = SequenceSearchResult {
					index:         Some(chosen),
					confidence:    search.confidence,
					match_count:   Some(1),
					match_indices: vec![chosen],
					strategy:      search.strategy,
				};
			}
		}

		// ── Error: no match found ────────────────────────────────────────
		let found = match search.index {
			Some(idx) => idx,
			None => {
				if matches!(search.match_count, Some(c) if c > 1) {
					let previews_text = format_sequence_match_previews(
						&line_refs,
						&search.match_indices,
						search.match_count,
					);
					let strategy_hint = match &search.strategy {
						Some(s) => format!(" Matching strategy: {s:?}."),
						None => String::new(),
					};
					let preview_block = if let Some(p) = &previews_text {
						format!("\n\n{p}")
					} else {
						String::new()
					};
					return Err(EditError::AmbiguousMatch {
						file:     path.to_string(),
						count:    search.match_count.unwrap_or(2),
						previews: format!(
							"Found {} matches for the text in \
							 {path}.{strategy_hint}{preview_block}\n\nAdd more surrounding context or \
							 additional @@ anchors to make it unique.",
							search.match_count.unwrap_or(2),
						),
					});
				}
				let pattern_refs = to_refs(&pattern);
				let closest = find_closest_sequence_match(
					&line_refs,
					&pattern_refs,
					Some(line_index),
					hunk.is_end_of_file,
				);
				let detail = if let Some(idx) = closest.index {
					let sim = (closest.confidence * 100.0).round() as u32;
					let preview = format_sequence_match_preview(&line_refs, idx);
					format!(
						"Failed to find expected lines in {path}:\n{}\n\nClosest match ({sim}% similar) \
						 near line {}:\n{preview}",
						hunk.old_lines.join("\n"),
						idx + 1,
					)
				} else {
					format!("Failed to find expected lines in {path}:\n{}", hunk.old_lines.join("\n"))
				};
				return Err(EditError::NoMatch { file: path.to_string(), detail });
			},
		};

		// Fuzzy-dominant warning.
		if search.strategy == Some(crate::fuzzy::SequenceMatchStrategy::FuzzyDominant) {
			let sim = (search.confidence * 100.0).round() as u32;
			warnings.push(format!(
				"Dominant fuzzy match selected in {path} near line {} ({sim}% similar).",
				found + 1,
			));
		}

		// Reject remaining ambiguity.
		if matches!(search.match_count, Some(c) if c > 1) {
			let previews_text =
				format_sequence_match_previews(&line_refs, &search.match_indices, search.match_count);
			let strategy_hint = match &search.strategy {
				Some(s) => format!(" Matching strategy: {s:?}."),
				None => String::new(),
			};
			let preview_block = if let Some(p) = &previews_text {
				format!("\n\n{p}")
			} else {
				String::new()
			};
			return Err(EditError::AmbiguousMatch {
				file:     path.to_string(),
				count:    search.match_count.unwrap_or(2),
				previews: format!(
					"Found {} matches for the text in {path}.{strategy_hint}{preview_block}\n\nAdd \
					 more surrounding context or additional @@ anchors to make it unique.",
					search.match_count.unwrap_or(2),
				),
			});
		}

		// Extra ambiguity check for simple diffs without disambiguation signals.
		if hunk.change_context.is_none()
			&& !hunk.has_context_lines
			&& !hunk.is_end_of_file
			&& line_hint.is_none()
		{
			let pattern_refs = to_refs(&pattern);
			let second = seek_sequence(&line_refs, &pattern_refs, found + 1, false, allow_fuzzy);
			if second.index.is_some() {
				let preview1 = format_sequence_match_preview(&line_refs, found);
				let preview2 =
					format_sequence_match_preview(&line_refs, second.index.expect("checked is_some"));
				return Err(EditError::AmbiguousMatch {
					file:     path.to_string(),
					count:    2,
					previews: format!(
						"Found 2 occurrences in {path}:\n\n{preview1}\n\n{preview2}\n\nAdd more context \
						 lines to disambiguate."
					),
				});
			}
		}

		// ── Build replacement ────────────────────────────────────────────
		let actual_matched = &original_lines[found..found + pattern.len()];

		// Skip no-op hunks (pure context, old == new).
		let is_noop = pattern.len() == new_slice.len()
			&& pattern.iter().zip(new_slice.iter()).all(|(a, b)| a == b);
		if is_noop {
			line_index = found + pattern.len();
			continue;
		}

		let pattern_refs = to_refs(&pattern);
		let actual_refs: Vec<&str> = actual_matched.iter().map(String::as_str).collect();
		let new_refs: Vec<&str> = new_slice.iter().map(String::as_str).collect();
		let adjusted = adjust_lines_indentation(&pattern_refs, &actual_refs, &new_refs);

		replacements.push(Replacement {
			start_index: found,
			old_len:     pattern.len(),
			new_lines:   adjusted,
		});
		line_index = found + pattern.len();
	}

	// Sort and check for overlaps.
	replacements.sort_by_key(|r| r.start_index);
	for window in replacements.windows(2) {
		let left = &window[0];
		let right = &window[1];
		let left_end = left.start_index + left.old_len;
		if right.start_index < left_end {
			let format_range = |r: &Replacement| -> String {
				if r.old_len == 0 {
					format!("{} (insertion)", r.start_index + 1)
				} else {
					format!("{}-{}", r.start_index + 1, r.start_index + r.old_len)
				}
			};
			return Err(EditError::OverlappingHunks {
				file:   path.to_string(),
				range1: format_range(left),
				range2: format_range(right),
			});
		}
	}

	Ok((replacements, warnings))
}

// ─── Indentation adjustment ──────────────────────────────────────────────────

/// Adjust indentation of `new_lines` to match the delta between
/// `pattern_lines` and `actual_lines`.
///
/// Mirrors the legacy coding-agent indentation behavior:
/// - preserves exact/context line indentation when possible
/// - handles tab↔space conversion patterns
/// - applies a safe uniform delta only for minimally-indented new lines
fn adjust_lines_indentation(
	pattern_lines: &[&str],
	actual_lines: &[&str],
	new_lines: &[&str],
) -> Vec<String> {
	if pattern_lines.is_empty() || actual_lines.is_empty() || new_lines.is_empty() {
		return new_lines.iter().map(|s| s.to_string()).collect();
	}

	// Exact match → preserve agent's intended changes.
	if pattern_lines.len() == actual_lines.len()
		&& pattern_lines
			.iter()
			.zip(actual_lines.iter())
			.all(|(a, b)| a == b)
	{
		return new_lines.iter().map(|s| s.to_string()).collect();
	}

	// Pure indentation change → return as-is.
	if pattern_lines.len() == new_lines.len()
		&& pattern_lines
			.iter()
			.zip(new_lines.iter())
			.all(|(a, b)| a.trim() == b.trim())
	{
		return new_lines.iter().map(|s| s.to_string()).collect();
	}

	// Detect dominant indent char from actual content.
	let mut indent_char = ' ';
	for line in actual_lines {
		let ws = get_leading_whitespace(line);
		if !ws.is_empty() {
			indent_char = ws.chars().next().unwrap_or(' ');
			break;
		}
	}

	let mut pattern_tab_only = true;
	let mut actual_space_only = true;
	let mut pattern_space_only = true;
	let mut actual_tab_only = true;
	let mut pattern_mixed = false;
	let mut actual_mixed = false;

	for line in pattern_lines {
		if line.trim().is_empty() {
			continue;
		}
		let ws = get_leading_whitespace(line);
		if ws.contains(' ') {
			pattern_tab_only = false;
		}
		if ws.contains('\t') {
			pattern_space_only = false;
		}
		if ws.contains(' ') && ws.contains('\t') {
			pattern_mixed = true;
		}
	}

	for line in actual_lines {
		if line.trim().is_empty() {
			continue;
		}
		let ws = get_leading_whitespace(line);
		if ws.contains('\t') {
			actual_space_only = false;
		}
		if ws.contains(' ') {
			actual_tab_only = false;
		}
		if ws.contains(' ') && ws.contains('\t') {
			actual_mixed = true;
		}
	}

	// Pattern uses tabs, actual uses spaces: infer tab width.
	if !pattern_mixed && !actual_mixed && pattern_tab_only && actual_space_only {
		let mut ratio: Option<usize> = None;
		let mut consistent = true;
		for (pattern_line, actual_line) in pattern_lines.iter().zip(actual_lines.iter()) {
			if pattern_line.trim().is_empty() || actual_line.trim().is_empty() {
				continue;
			}
			let pattern_indent = count_leading_whitespace(pattern_line);
			let actual_indent = count_leading_whitespace(actual_line);
			if pattern_indent == 0 {
				continue;
			}
			if !actual_indent.is_multiple_of(pattern_indent) {
				consistent = false;
				break;
			}
			let next_ratio = actual_indent / pattern_indent;
			match ratio {
				None => ratio = Some(next_ratio),
				Some(value) if value == next_ratio => {},
				Some(_) => {
					consistent = false;
					break;
				},
			}
		}
		if consistent
			&& let Some(value) = ratio
			&& value > 0
		{
			let converted = convert_leading_tabs_to_spaces(&new_lines.join("\n"), value);
			return converted.split('\n').map(str::to_string).collect();
		}
	}

	// Pattern uses spaces, actual uses tabs: infer spaces = tabs * width + offset.
	if !pattern_mixed && !actual_mixed && pattern_space_only && actual_tab_only {
		let mut samples = HashMap::<usize, usize>::new(); // tabs -> spaces
		let mut consistent = true;
		for (pattern_line, actual_line) in pattern_lines.iter().zip(actual_lines.iter()) {
			if pattern_line.trim().is_empty() || actual_line.trim().is_empty() {
				continue;
			}
			let spaces = count_leading_whitespace(pattern_line);
			let tabs = count_leading_whitespace(actual_line);
			if tabs == 0 {
				continue;
			}
			match samples.get(&tabs) {
				Some(existing) if *existing != spaces => {
					consistent = false;
					break;
				},
				_ => {
					samples.insert(tabs, spaces);
				},
			}
		}

		if consistent && !samples.is_empty() {
			let mut tab_width: Option<isize> = None;
			let mut offset: isize = 0;

			if samples.len() == 1 {
				if let Some((&tabs, &spaces)) = samples.iter().next()
					&& spaces % tabs == 0
				{
					tab_width = Some((spaces / tabs) as isize);
				}
			} else {
				let entries: Vec<(usize, usize)> = samples.iter().map(|(t, s)| (*t, *s)).collect();
				let (t1, s1) = entries[0];
				let (t2, s2) = entries[1];
				if t1 != t2 {
					let numerator = s2 as isize - s1 as isize;
					let denominator = t2 as isize - t1 as isize;
					if denominator != 0 && numerator % denominator == 0 {
						let w = numerator / denominator;
						if w > 0 {
							let b = s1 as isize - t1 as isize * w;
							let mut valid = true;
							for (tabs, spaces) in &samples {
								if *tabs as isize * w + b != *spaces as isize {
									valid = false;
									break;
								}
							}
							if valid {
								tab_width = Some(w);
								offset = b;
							}
						}
					}
				}
			}

			if let Some(width) = tab_width
				&& width > 0
			{
				return new_lines
					.iter()
					.map(|line| {
						if line.trim().is_empty() {
							return (*line).to_string();
						}
						let ws = count_leading_whitespace(line) as isize;
						if ws == 0 {
							return (*line).to_string();
						}
						let adjusted = ws - offset;
						if adjusted >= 0 && adjusted % width == 0 {
							let tab_count = adjusted / width;
							return format!("{}{}", "\t".repeat(tab_count as usize), &line[ws as usize..]);
						}
						let tab_count = adjusted.div_euclid(width);
						let remainder = adjusted - tab_count * width;
						if tab_count >= 0 && remainder >= 0 {
							return format!(
								"{}{}{}",
								"\t".repeat(tab_count as usize),
								" ".repeat(remainder as usize),
								&line[ws as usize..]
							);
						}
						(*line).to_string()
					})
					.collect();
			}
		}
	}

	// Build map from trimmed content to actual lines for context preservation.
	let mut content_to_actual_lines = HashMap::<String, Vec<String>>::new();
	for line in actual_lines {
		let trimmed = line.trim();
		if trimmed.is_empty() {
			continue;
		}
		content_to_actual_lines
			.entry(trimmed.to_string())
			.or_default()
			.push((*line).to_string());
	}

	let mut pattern_min = usize::MAX;
	for line in pattern_lines {
		if line.trim().is_empty() {
			continue;
		}
		pattern_min = pattern_min.min(count_leading_whitespace(line));
	}
	if pattern_min == usize::MAX {
		pattern_min = 0;
	}

	let mut deltas = Vec::<isize>::new();
	for (pattern_line, actual_line) in pattern_lines.iter().zip(actual_lines.iter()) {
		if pattern_line.trim().is_empty() || actual_line.trim().is_empty() {
			continue;
		}
		let p = count_leading_whitespace(pattern_line) as isize;
		let a = count_leading_whitespace(actual_line) as isize;
		deltas.push(a - p);
	}
	let delta = if deltas.is_empty() {
		None
	} else if deltas.iter().all(|value| *value == deltas[0]) {
		Some(deltas[0])
	} else {
		None
	};

	let mut used_actual_lines = HashMap::<String, usize>::new();
	new_lines
		.iter()
		.map(|new_line| {
			if new_line.trim().is_empty() {
				return (*new_line).to_string();
			}

			let trimmed = new_line.trim();
			if let Some(matching_actual_lines) = content_to_actual_lines.get(trimmed) {
				if matching_actual_lines.len() == 1 {
					return matching_actual_lines[0].clone();
				}
				if matching_actual_lines.iter().any(|line| line == new_line) {
					return (*new_line).to_string();
				}
				let used_count = *used_actual_lines.get(trimmed).unwrap_or(&0);
				if used_count < matching_actual_lines.len() {
					used_actual_lines.insert(trimmed.to_string(), used_count + 1);
					return matching_actual_lines[used_count].clone();
				}
			}

			if let Some(indent_delta) = delta
				&& indent_delta != 0
			{
				let new_indent = count_leading_whitespace(new_line);
				if new_indent == pattern_min {
					if indent_delta > 0 {
						return format!(
							"{}{}",
							indent_char.to_string().repeat(indent_delta as usize),
							new_line
						);
					}
					let to_remove = (-indent_delta as usize).min(new_indent);
					return new_line[to_remove..].to_string();
				}
			}

			(*new_line).to_string()
		})
		.collect()
}

// ─── Sequence search helpers ─────────────────────────────────────────────────

/// Convert owned strings to borrowed references for the fuzzy API.
fn to_refs(lines: &[String]) -> Vec<&str> {
	lines.iter().map(String::as_str).collect()
}

/// Extract a 0-indexed hint from the hunk's `old_start_line`, returning `None`
/// if the hint is behind `current_index`.
fn get_hunk_hint_index(hunk: &DiffHunk, current_index: usize) -> Option<usize> {
	let line = hunk.old_start_line?;
	let idx = line.saturating_sub(1);
	if idx >= current_index {
		Some(idx)
	} else {
		None
	}
}

/// Find a sequence with optional hint position, retrying from hint and from
/// the start of the file when the primary search fails.
fn find_sequence_with_hint(
	lines: &[&str],
	pattern: &[&str],
	current_index: usize,
	hint_index: Option<usize>,
	eof: bool,
	allow_fuzzy: bool,
) -> SequenceSearchResult {
	let primary = seek_sequence(lines, pattern, current_index, eof, allow_fuzzy);

	// If ambiguous, try from hint to narrow down.
	if matches!(primary.match_count, Some(c) if c > 1)
		&& let Some(hint) = hint_index
		&& hint != current_index
	{
		let hinted = seek_sequence(lines, pattern, hint, eof, allow_fuzzy);
		if hinted.index.is_some() && matches!(hinted.match_count, None | Some(0 | 1)) {
			return hinted;
		}
		if matches!(hinted.match_count, Some(c) if c > 1) {
			return hinted;
		}
	}

	if primary.index.is_some() || matches!(primary.match_count, Some(c) if c > 1) {
		return primary;
	}

	// Retry from hint.
	if let Some(hint) = hint_index
		&& hint != current_index
	{
		let hinted = seek_sequence(lines, pattern, hint, eof, allow_fuzzy);
		if hinted.index.is_some() || matches!(hinted.match_count, Some(c) if c > 1) {
			return hinted;
		}
	}

	// Last resort: search from beginning.
	if current_index != 0 {
		let from_start = seek_sequence(lines, pattern, 0, eof, allow_fuzzy);
		if from_start.index.is_some() || matches!(from_start.match_count, Some(c) if c > 1) {
			return from_start;
		}
	}

	primary
}

/// Attempt to find the hunk's old_lines via fallback variants when the primary
/// search and context matching both failed.
fn attempt_sequence_fallback(
	lines: &[&str],
	hunk: &DiffHunk,
	current_index: usize,
	line_hint: Option<usize>,
	allow_fuzzy: bool,
	allow_aggressive: bool,
) -> Option<usize> {
	if hunk.old_lines.is_empty() {
		return None;
	}

	let match_hint = get_hunk_hint_index(hunk, current_index);
	let pattern_refs = to_refs(&hunk.old_lines);
	let result = find_sequence_with_hint(
		lines,
		&pattern_refs,
		current_index,
		match_hint.or(line_hint),
		false,
		allow_fuzzy,
	);
	if result.index.is_some() && matches!(result.match_count, None | Some(0 | 1)) {
		let found = result.index.expect("checked is_some");
		// Verify uniqueness.
		let next = found + 1;
		if next <= lines.len().saturating_sub(hunk.old_lines.len()) {
			let second = seek_sequence(lines, &pattern_refs, next, false, allow_fuzzy);
			if second.index.is_some() {
				return None;
			}
		}
		return Some(found);
	}

	for variant in filter_fallback_variants(&build_fallback_variants(hunk), allow_aggressive) {
		if variant.old_lines.is_empty() {
			continue;
		}
		let vrefs = to_refs(&variant.old_lines);
		let vr = find_sequence_with_hint(
			lines,
			&vrefs,
			current_index,
			match_hint.or(line_hint),
			false,
			allow_fuzzy,
		);
		if vr.index.is_some() && matches!(vr.match_count, None | Some(0 | 1)) {
			return vr.index;
		}
	}

	None
}

// ─── Context search helpers ──────────────────────────────────────────────────

/// Find hierarchical context in file lines.
///
/// Handles three formats:
/// 1. Simple context: `"function foo"` — find this line.
/// 2. Hierarchical (newline): `"class Foo\nmethod"` — find class, then method
///    after it.
/// 3. Hierarchical (space): `"class Foo method"` — try literal first, then
///    split and search.
fn find_hierarchical_context(
	lines: &[&str],
	context: &str,
	start_from: usize,
	line_hint: Option<usize>,
	allow_fuzzy: bool,
) -> ContextLineResult {
	// Newline-separated hierarchical context.
	if context.contains('\n') {
		let parts: Vec<&str> = context
			.split('\n')
			.map(str::trim)
			.filter(|p| !p.is_empty())
			.collect();
		let mut current_start = start_from;

		for (i, part) in parts.iter().enumerate() {
			let is_last = i == parts.len() - 1;
			let result = find_context_line(lines, part, current_start, allow_fuzzy);

			if matches!(result.match_count, Some(c) if c > 1) {
				if is_last && let Some(hint) = line_hint {
					let hint_start = hint.saturating_sub(1).max(current_start);
					let hinted = find_context_line(lines, part, hint_start, allow_fuzzy);
					if hinted.index.is_some() {
						return ContextLineResult {
							match_count: Some(1),
							match_indices: hinted.index.into_iter().collect(),
							..hinted
						};
					}
				}
				return ContextLineResult {
					index:         None,
					confidence:    result.confidence,
					match_count:   result.match_count,
					match_indices: result.match_indices,
					strategy:      result.strategy,
				};
			}

			if result.index.is_none() {
				if is_last && let Some(hint) = line_hint {
					let hint_start = hint.saturating_sub(1).max(current_start);
					let hinted = find_context_line(lines, part, hint_start, allow_fuzzy);
					if hinted.index.is_some() {
						return ContextLineResult {
							match_count: Some(1),
							match_indices: hinted.index.into_iter().collect(),
							..hinted
						};
					}
				}
				return ContextLineResult {
					index:         None,
					confidence:    result.confidence,
					match_count:   None,
					match_indices: vec![],
					strategy:      None,
				};
			}

			if is_last {
				return result;
			}
			current_start = result.index.expect("checked is_some above") + 1;
		}
		return ContextLineResult {
			index:         None,
			confidence:    0.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      None,
		};
	}

	// Try space-separated hierarchical matching (before literal).
	let space_parts: Vec<&str> = context
		.split_whitespace()
		.filter(|p| !p.is_empty())
		.collect();
	let has_signature_chars = context.contains('(')
		|| context.contains(')')
		|| context.contains('{')
		|| context.contains('}')
		|| context.contains('[')
		|| context.contains(']');

	if !has_signature_chars && space_parts.len() > 2 {
		let outer = space_parts[..space_parts.len() - 1].join(" ");
		let inner = space_parts[space_parts.len() - 1];
		let outer_result = find_context_line(lines, &outer, start_from, allow_fuzzy);
		if matches!(outer_result.match_count, Some(c) if c > 1) {
			return ContextLineResult {
				index:         None,
				confidence:    outer_result.confidence,
				match_count:   outer_result.match_count,
				match_indices: outer_result.match_indices,
				strategy:      outer_result.strategy,
			};
		}
		if let Some(outer_idx) = outer_result.index {
			let inner_result = find_context_line(lines, inner, outer_idx + 1, allow_fuzzy);
			if inner_result.index.is_some() {
				return if matches!(inner_result.match_count, Some(c) if c > 1) {
					ContextLineResult {
						match_count: Some(1),
						match_indices: inner_result.index.into_iter().collect(),
						..inner_result
					}
				} else {
					inner_result
				};
			}
		}
	}

	// Literal context search.
	let result = find_context_line(lines, context, start_from, allow_fuzzy);

	// If ambiguous or missing and we have a hint, try from hint.
	if (result.index.is_none() || matches!(result.match_count, Some(c) if c > 1))
		&& let Some(hint) = line_hint
	{
		let hint_start = hint.saturating_sub(1);
		let hinted = find_context_line(lines, context, hint_start, allow_fuzzy);
		if hinted.index.is_some() {
			return ContextLineResult {
				match_count: Some(1),
				match_indices: hinted.index.into_iter().collect(),
				..hinted
			};
		}
	}

	// Unique match.
	if result.index.is_some() && matches!(result.match_count, None | Some(0 | 1)) {
		return result;
	}
	if matches!(result.match_count, Some(c) if c > 1) {
		return result;
	}

	// Retry from beginning.
	if result.index.is_none() && start_from != 0 {
		let from_start = find_context_line(lines, context, 0, allow_fuzzy);
		if from_start.index.is_some() && matches!(from_start.match_count, None | Some(0 | 1)) {
			return from_start;
		}
		if matches!(from_start.match_count, Some(c) if c > 1) {
			return from_start;
		}
	}

	// Fallback: space-separated hierarchical matching.
	if !has_signature_chars && space_parts.len() > 1 {
		let outer = space_parts[..space_parts.len() - 1].join(" ");
		let inner = space_parts[space_parts.len() - 1];
		let outer_result = find_context_line(lines, &outer, start_from, allow_fuzzy);
		if matches!(outer_result.match_count, Some(c) if c > 1) {
			return ContextLineResult {
				index:         None,
				confidence:    outer_result.confidence,
				match_count:   outer_result.match_count,
				match_indices: outer_result.match_indices,
				strategy:      outer_result.strategy,
			};
		}
		if outer_result.index.is_none() {
			return ContextLineResult {
				index:         None,
				confidence:    outer_result.confidence,
				match_count:   None,
				match_indices: vec![],
				strategy:      None,
			};
		}
		let outer_idx = outer_result.index.expect("checked is_some");
		let inner_result = find_context_line(lines, inner, outer_idx + 1, allow_fuzzy);
		if inner_result.index.is_some() {
			return if matches!(inner_result.match_count, Some(c) if c > 1) {
				ContextLineResult {
					match_count: Some(1),
					match_indices: inner_result.index.into_iter().collect(),
					..inner_result
				}
			} else {
				inner_result
			};
		}
		if matches!(inner_result.match_count, Some(c) if c > 1) {
			return ContextLineResult {
				match_count: Some(1),
				match_indices: inner_result.index.into_iter().collect(),
				..inner_result
			};
		}
	}

	result
}

/// Find a line near `context_index` by trimmed content comparison.
/// Prefers forward matches; falls back to searching backward.
fn find_context_relative_match(
	lines: &[&str],
	pattern_line: &str,
	context_index: usize,
	prefer_second_forward: bool,
) -> Option<usize> {
	let trimmed = pattern_line.trim();
	let mut forward_matches = Vec::new();
	for i in (context_index + 1)..lines.len() {
		if lines[i].trim() == trimmed {
			forward_matches.push(i);
		}
	}
	if !forward_matches.is_empty() {
		if prefer_second_forward && forward_matches.len() > 1 {
			return Some(forward_matches[1]);
		}
		return Some(forward_matches[0]);
	}
	// Search backward.
	(0..context_index)
		.rev()
		.find(|&i| lines[i].trim() == trimmed)
}

/// Choose the match closest to `hint_index` within `window`, returning `Some`
/// only when exactly one candidate falls inside the window.
fn choose_hinted_match(
	match_indices: &[usize],
	hint_index: Option<usize>,
	window: usize,
) -> Option<usize> {
	let hint = hint_index?;
	if match_indices.is_empty() {
		return None;
	}
	let candidates: Vec<usize> = match_indices
		.iter()
		.copied()
		.filter(|&idx| (idx as isize - hint as isize).unsigned_abs() <= window)
		.collect();
	if candidates.len() == 1 {
		Some(candidates[0])
	} else {
		None
	}
}

// ─── Fallback variant builders ───────────────────────────────────────────────

/// Trim common prefix/suffix context from old/new lines.
fn trim_common_context(old: &[String], new: &[String]) -> Option<HunkVariant> {
	let mut start = 0;
	let mut end_old = old.len();
	let mut end_new = new.len();

	while start < end_old && start < end_new && old[start] == new[start] {
		start += 1;
	}
	while end_old > start && end_new > start && old[end_old - 1] == new[end_new - 1] {
		end_old -= 1;
		end_new -= 1;
	}

	if start == 0 && end_old == old.len() && end_new == new.len() {
		return None;
	}

	let trimmed_old = old[start..end_old].to_vec();
	let trimmed_new = new[start..end_new].to_vec();
	if trimmed_old.is_empty() && trimmed_new.is_empty() {
		return None;
	}
	Some(HunkVariant {
		old_lines: trimmed_old,
		new_lines: trimmed_new,
		kind:      HunkVariantKind::TrimCommon,
	})
}

/// Collapse consecutive duplicate shared lines.
fn collapse_consecutive_shared_lines(old: &[String], new: &[String]) -> Option<HunkVariant> {
	let shared: std::collections::HashSet<&str> = old
		.iter()
		.filter(|l| new.iter().any(|n| n == *l))
		.map(String::as_str)
		.collect();

	let collapse = |lines: &[String]| -> Vec<String> {
		let mut out = Vec::new();
		let mut i = 0;
		while i < lines.len() {
			out.push(lines[i].clone());
			let mut j = i + 1;
			while j < lines.len() && lines[j] == lines[i] && shared.contains(lines[i].as_str()) {
				j += 1;
			}
			i = j;
		}
		out
	};

	let collapsed_old = collapse(old);
	let collapsed_new = collapse(new);
	if collapsed_old.len() == old.len() && collapsed_new.len() == new.len() {
		return None;
	}
	Some(HunkVariant {
		old_lines: collapsed_old,
		new_lines: collapsed_new,
		kind:      HunkVariantKind::DedupeShared,
	})
}

/// Collapse repeated blocks of shared lines.
fn collapse_repeated_blocks(old: &[String], new: &[String]) -> Option<HunkVariant> {
	let shared: std::collections::HashSet<&str> = old
		.iter()
		.filter(|l| new.iter().any(|n| n == *l))
		.map(String::as_str)
		.collect();

	let collapse = |lines: &[String]| -> Vec<String> {
		let mut output = lines.to_vec();
		let mut changed = false;
		let mut i = 0;
		while i < output.len() {
			let mut collapsed = false;
			let max_size = (output.len() - i) / 2;
			for size in (2..=max_size).rev() {
				if i + size * 2 > output.len() {
					continue;
				}
				let first = &output[i..i + size];
				let second = &output[i + size..i + size * 2];
				if first.len() != second.len() {
					continue;
				}
				if !first.iter().all(|l| shared.contains(l.as_str())) {
					continue;
				}
				if first == second {
					output.drain(i + size..i + size * 2);
					changed = true;
					collapsed = true;
					break;
				}
			}
			if !collapsed {
				i += 1;
			}
		}
		if changed { output } else { lines.to_vec() }
	};

	let collapsed_old = collapse(old);
	let collapsed_new = collapse(new);
	if collapsed_old.len() == old.len() && collapsed_new.len() == new.len() {
		return None;
	}
	Some(HunkVariant {
		old_lines: collapsed_old,
		new_lines: collapsed_new,
		kind:      HunkVariantKind::CollapseRepeated,
	})
}

/// Reduce to a single changed line when exactly one line differs.
fn reduce_to_single_line_change(old: &[String], new: &[String]) -> Option<HunkVariant> {
	if old.len() != new.len() || old.is_empty() {
		return None;
	}
	let mut changed_index: Option<usize> = None;
	for i in 0..old.len() {
		if old[i] != new[i] {
			if changed_index.is_some() {
				return None; // Multiple changes.
			}
			changed_index = Some(i);
		}
	}
	let idx = changed_index?;
	Some(HunkVariant {
		old_lines: vec![old[idx].clone()],
		new_lines: vec![new[idx].clone()],
		kind:      HunkVariantKind::SingleLine,
	})
}

/// Build fallback variants for a hunk, from least to most aggressive.
fn build_fallback_variants(hunk: &DiffHunk) -> Vec<HunkVariant> {
	let mut variants = Vec::new();
	let mut seen = std::collections::HashSet::new();

	let base_old = &hunk.old_lines;
	let base_new = &hunk.new_lines;

	let trimmed = trim_common_context(base_old, base_new);

	if let Some(v) = &trimmed {
		let key = format!("{}||{}", v.old_lines.join("\n"), v.new_lines.join("\n"));
		if !(v.old_lines.is_empty() && v.new_lines.is_empty()) && seen.insert(key) {
			variants.push(v.clone());
		}
	}

	let dedup_src_old = trimmed
		.as_ref()
		.map_or(base_old.as_slice(), |t| &t.old_lines);
	let dedup_src_new = trimmed
		.as_ref()
		.map_or(base_new.as_slice(), |t| &t.new_lines);
	let deduped = collapse_consecutive_shared_lines(dedup_src_old, dedup_src_new);

	if let Some(v) = &deduped {
		let key = format!("{}||{}", v.old_lines.join("\n"), v.new_lines.join("\n"));
		if !(v.old_lines.is_empty() && v.new_lines.is_empty()) && seen.insert(key) {
			variants.push(v.clone());
		}
	}

	let collapse_src_old = deduped.as_ref().map_or(dedup_src_old, |d| &d.old_lines);
	let collapse_src_new = deduped.as_ref().map_or(dedup_src_new, |d| &d.new_lines);
	let collapsed = collapse_repeated_blocks(collapse_src_old, collapse_src_new);

	if let Some(v) = &collapsed {
		let key = format!("{}||{}", v.old_lines.join("\n"), v.new_lines.join("\n"));
		if !(v.old_lines.is_empty() && v.new_lines.is_empty()) && seen.insert(key) {
			variants.push(v.clone());
		}
	}

	let single_src_old = trimmed
		.as_ref()
		.map_or(base_old.as_slice(), |t| &t.old_lines);
	let single_src_new = trimmed
		.as_ref()
		.map_or(base_new.as_slice(), |t| &t.new_lines);
	let single = reduce_to_single_line_change(single_src_old, single_src_new);

	if let Some(v) = &single {
		let key = format!("{}||{}", v.old_lines.join("\n"), v.new_lines.join("\n"));
		if !(v.old_lines.is_empty() && v.new_lines.is_empty()) && seen.insert(key) {
			variants.push(v.clone());
		}
	}

	variants
}

/// Filter variants: aggressive kinds (collapse-repeated, single-line) are only
/// allowed when `allow_aggressive` is true.
fn filter_fallback_variants(variants: &[HunkVariant], allow_aggressive: bool) -> Vec<&HunkVariant> {
	if allow_aggressive {
		return variants.iter().collect();
	}
	variants
		.iter()
		.filter(|v| {
			v.kind != HunkVariantKind::CollapseRepeated && v.kind != HunkVariantKind::SingleLine
		})
		.collect()
}

// ─── Preview formatting ─────────────────────────────────────────────────────

/// Format a short preview of lines around `start_idx` for error messages.
fn format_sequence_match_preview(lines: &[&str], start_idx: usize) -> String {
	let start = start_idx.saturating_sub(MATCH_PREVIEW_CONTEXT);
	let end = (start_idx + MATCH_PREVIEW_CONTEXT + 1).min(lines.len());
	lines[start..end]
		.iter()
		.enumerate()
		.map(|(i, line)| {
			let num = start + i + 1;
			let truncated = if line.len() > MATCH_PREVIEW_MAX_LEN {
				format!("{}…", &line[..MATCH_PREVIEW_MAX_LEN - 1])
			} else {
				line.to_string()
			};
			format!("  {num} | {truncated}")
		})
		.collect::<Vec<_>>()
		.join("\n")
}

/// Format previews for multiple match locations.
fn format_sequence_match_previews(
	lines: &[&str],
	match_indices: &[usize],
	match_count: Option<usize>,
) -> Option<String> {
	if match_indices.is_empty() {
		return None;
	}
	let previews: Vec<String> = match_indices
		.iter()
		.map(|&idx| format_sequence_match_preview(lines, idx))
		.collect();
	let more = match match_count {
		Some(total) if total > match_indices.len() => {
			format!(" (showing first {} of {total})", match_indices.len())
		},
		_ => String::new(),
	};
	Some(format!("{}{more}", previews.join("\n\n")))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;
	use crate::fs::InMemoryFs;

	fn make_input(
		path: &str,
		op: Operation,
		diff: Option<&str>,
		rename: Option<&str>,
	) -> PatchInput {
		PatchInput {
			path: path.to_string(),
			op,
			rename: rename.map(str::to_string),
			diff: diff.map(str::to_string),
		}
	}

	#[test]
	fn create_op_writes_file() {
		let fs = InMemoryFs::new();
		let input = make_input("hello.txt", Operation::Create, Some("hello world\n"), None);
		let result =
			apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("create should succeed");
		assert_eq!(result.change.op, ChangeOp::Create);
		assert_eq!(fs.get("hello.txt").expect("file should exist"), "hello world\n");
		assert!(result.warnings.is_empty());
	}

	#[test]
	fn create_op_adds_trailing_newline() {
		let fs = InMemoryFs::new();
		let input = make_input("no_nl.txt", Operation::Create, Some("content"), None);
		let result =
			apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("create should succeed");
		assert_eq!(result.change.new_content.expect("should have content"), "content\n");
	}

	#[test]
	fn create_op_rejects_existing_file() {
		let fs = InMemoryFs::with_files([("exists.txt", "data")]);
		let input = make_input("exists.txt", Operation::Create, Some("new"), None);
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(matches!(err, EditError::FileAlreadyExists { .. }));
	}

	#[test]
	fn delete_op_removes_file() {
		let fs = InMemoryFs::with_files([("gone.txt", "bye")]);
		let input = make_input("gone.txt", Operation::Delete, None, None);
		let result =
			apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("delete should succeed");
		assert_eq!(result.change.op, ChangeOp::Delete);
		assert_eq!(
			result
				.change
				.old_content
				.expect("should capture old content"),
			"bye"
		);
		assert!(fs.get("gone.txt").is_none());
	}

	#[test]
	fn delete_op_file_not_found() {
		let fs = InMemoryFs::new();
		let input = make_input("nope.txt", Operation::Delete, None, None);
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(matches!(err, EditError::FileNotFound { .. }));
	}

	#[test]
	fn update_simple_hunk() {
		let fs = InMemoryFs::with_files([("test.rs", "fn main() {\n    println!(\"hello\");\n}\n")]);
		let diff = "\
-    println!(\"hello\");
+    println!(\"world\");";
		let input = make_input("test.rs", Operation::Update, Some(diff), None);
		let result =
			apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("update should succeed");
		assert_eq!(result.change.op, ChangeOp::Update);
		let new = fs.get("test.rs").expect("file should exist");
		assert!(new.contains("world"));
		assert!(!new.contains("hello"));
	}

	#[test]
	fn update_with_context_lines() {
		let original = "line1\nline2\nline3\nline4\n";
		let fs = InMemoryFs::with_files([("ctx.txt", original)]);
		let diff = "\
 line1
 line2
-line3
+replaced
 line4";
		let input = make_input("ctx.txt", Operation::Update, Some(diff), None);
		let result = apply_patch(&input, &fs, ApplyPatchOptions::default())
			.expect("context update should succeed");
		let new = fs.get("ctx.txt").expect("file should exist");
		assert!(new.contains("replaced"));
		assert!(!new.contains("line3"));
		assert!(new.contains("line1"));
		assert!(new.contains("line4"));
		assert!(result.warnings.is_empty());
	}

	#[test]
	fn overlapping_hunks_error() {
		// Two hunks that both target line2→replaced.
		let original = "line1\nline2\nline3\n";
		let fs = InMemoryFs::with_files([("overlap.txt", original)]);
		// A diff that produces two overlapping replacements at the same location:
		// Both hunks include the full file so their replacements overlap.
		let diff = "\
@@ -1,2 +1,2 @@
-line1
-line2
+a
+b
@@ -2,2 +2,2 @@
-line2
-line3
+c
+d";
		let input = make_input("overlap.txt", Operation::Update, Some(diff), None);
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(matches!(err, EditError::OverlappingHunks { .. }));
	}

	#[test]
	fn rename_to_same_path_error() {
		let fs = InMemoryFs::with_files([("file.txt", "data\n")]);
		let input =
			make_input("file.txt", Operation::Update, Some("-data\n+new\n"), Some("file.txt"));
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(matches!(err, EditError::SamePathRename));
	}

	#[test]
	fn update_with_rename() {
		let fs = InMemoryFs::with_files([("old.txt", "hello\n")]);
		let diff = "-hello\n+goodbye";
		let input = make_input("old.txt", Operation::Update, Some(diff), Some("new.txt"));
		let result =
			apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("rename should succeed");
		assert_eq!(result.change.new_path, Some("new.txt".to_string()));
		assert!(fs.get("old.txt").is_none());
		assert!(fs.get("new.txt").is_some());
	}

	#[test]
	fn empty_diff_on_update_errors() {
		let fs = InMemoryFs::with_files([("e.txt", "content\n")]);
		let input = make_input("e.txt", Operation::Update, Some(""), None);
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(matches!(err, EditError::ParseError { .. }));
	}

	#[test]
	fn preview_does_not_mutate() {
		let fs = InMemoryFs::with_files([("ro.txt", "hello\n")]);
		let diff = "-hello\n+world";
		let input = make_input("ro.txt", Operation::Update, Some(diff), None);
		let result =
			preview_patch(&input, &fs, ApplyPatchOptions::default()).expect("preview should succeed");
		assert_eq!(result.change.op, ChangeOp::Update);
		// File should not be mutated.
		assert_eq!(fs.get("ro.txt").expect("file untouched"), "hello\n");
	}

	#[test]
	fn trailing_newline_preserved() {
		let fs = InMemoryFs::with_files([("nl.txt", "a\nb\n")]);
		let diff = "-b\n+c";
		let input = make_input("nl.txt", Operation::Update, Some(diff), None);
		apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("should succeed");
		let content = fs.get("nl.txt").expect("file should exist");
		assert!(content.ends_with('\n'), "trailing newline should be preserved");
	}

	#[test]
	fn no_trailing_newline_preserved() {
		let fs = InMemoryFs::with_files([("no_nl.txt", "a\nb")]);
		let diff = "-b\n+c";
		let input = make_input("no_nl.txt", Operation::Update, Some(diff), None);
		apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("should succeed");
		let content = fs.get("no_nl.txt").expect("file should exist");
		assert!(!content.ends_with('\n'), "no trailing newline should be preserved");
	}

	#[test]
	fn bom_preserved_on_update() {
		let content_with_bom = "\u{FEFF}hello\nworld\n";
		let fs = InMemoryFs::with_files([("bom.txt", content_with_bom)]);
		let diff = "-hello\n+goodbye";
		let input = make_input("bom.txt", Operation::Update, Some(diff), None);
		apply_patch(&input, &fs, ApplyPatchOptions::default()).expect("should succeed");
		let result = fs.get("bom.txt").expect("file should exist");
		assert!(result.starts_with('\u{FEFF}'), "BOM should be preserved");
		assert!(result.contains("goodbye"));
	}

	#[test]
	fn character_match_multiple_occurrences_error() {
		let fs = InMemoryFs::with_files([("dup.txt", "foo\nbar\nfoo\n")]);
		// Single hunk, no context → character match. "foo" appears twice.
		let diff = "-foo\n+baz";
		let input = make_input("dup.txt", Operation::Update, Some(diff), None);
		let err = apply_patch(&input, &fs, ApplyPatchOptions::default()).unwrap_err();
		assert!(
			matches!(err, EditError::AmbiguousMatch { .. }),
			"expected AmbiguousMatch, got {err:?}"
		);
	}

	#[test]
	fn trim_common_context_basic() {
		let old = vec!["a".into(), "b".into(), "c".into()];
		let new = vec!["a".into(), "x".into(), "c".into()];
		let variant = trim_common_context(&old, &new).expect("should produce variant");
		assert_eq!(variant.old_lines, vec!["b"]);
		assert_eq!(variant.new_lines, vec!["x"]);
	}

	#[test]
	fn reduce_single_line_change_basic() {
		let old = vec!["a".into(), "b".into(), "c".into()];
		let new = vec!["a".into(), "x".into(), "c".into()];
		let variant = reduce_to_single_line_change(&old, &new).expect("should produce variant");
		assert_eq!(variant.old_lines, vec!["b"]);
		assert_eq!(variant.new_lines, vec!["x"]);
	}

	#[test]
	fn pure_addition_hunk() {
		let fs = InMemoryFs::with_files([("add.txt", "line1\nline2\n")]);
		let diff = "@@ -2,0 +2,1 @@\n+inserted";
		let input = make_input("add.txt", Operation::Update, Some(diff), None);
		let result = apply_patch(&input, &fs, ApplyPatchOptions::default())
			.expect("pure addition should succeed");
		let content = fs.get("add.txt").expect("file should exist");
		assert!(content.contains("inserted"));
		assert!(result.warnings.is_empty());
	}
}
