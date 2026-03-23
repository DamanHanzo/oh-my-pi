//! Fuzzy matching utilities for the edit tool.
//!
//! Provides both character-level and line-level fuzzy matching with progressive
//! fallback strategies for finding text in files.

use crate::normalize::{count_leading_whitespace, normalize_for_fuzzy, normalize_unicode};

// ───────────────────────────────────────────────────────────────────────────
// Constants
// ───────────────────────────────────────────────────────────────────────────

/// Default similarity threshold for fuzzy matching.
pub const DEFAULT_FUZZY_THRESHOLD: f64 = 0.95;

/// Threshold for sequence-based fuzzy matching.
pub const SEQUENCE_FUZZY_THRESHOLD: f64 = 0.92;

/// Fallback threshold for line-based matching.
pub const FALLBACK_THRESHOLD: f64 = 0.8;

/// Threshold for context line matching.
pub const CONTEXT_FUZZY_THRESHOLD: f64 = 0.8;

/// Minimum length for partial/substring matching.
pub const PARTIAL_MATCH_MIN_LENGTH: usize = 6;

/// Minimum ratio of pattern to line length for substring match.
pub const PARTIAL_MATCH_MIN_RATIO: f64 = 0.3;

/// Threshold for character-based fallback matching in `seek_sequence`.
const CHARACTER_MATCH_THRESHOLD: f64 = 0.92;

/// Minimum confidence for a fuzzy match to be considered "dominant" over
/// alternatives.
const DOMINANT_MIN: f64 = 0.97;

/// Minimum gap between best and second-best to declare dominance.
const DOMINANT_DELTA: f64 = 0.08;

/// Maximum number of match indices to track for ambiguity reporting.
const MAX_TRACKED_INDICES: usize = 5;
/// Context lines shown before/after an ambiguous exact occurrence preview.
const OCCURRENCE_PREVIEW_CONTEXT: usize = 5;
/// Maximum preview line length.
const OCCURRENCE_PREVIEW_MAX_LEN: usize = 80;

// ───────────────────────────────────────────────────────────────────────────
// Types
// ───────────────────────────────────────────────────────────────────────────

/// A single fuzzy match result with location and confidence.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
	/// The text that was actually matched in the content.
	pub actual_text: String,
	/// Byte offset of the match start within the content.
	pub start_index: usize,
	/// 1-indexed line number of the match start.
	pub start_line:  usize,
	/// Similarity score in `[0.0, 1.0]`.
	pub confidence:  f64,
}

/// Outcome of [`find_match`]: either an exact/fuzzy match, ambiguity info, or
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct MatchOutcome {
	/// The accepted match (exact or fuzzy).
	pub matched:             Option<FuzzyMatch>,
	/// The closest fuzzy candidate, even if not accepted.
	pub closest:             Option<FuzzyMatch>,
	/// Number of exact occurrences when ambiguous.
	pub occurrences:         Option<usize>,
	/// 1-indexed line numbers of exact occurrences (up to 5).
	pub occurrence_lines:    Vec<usize>,
	/// Preview snippets for exact occurrences (up to 5).
	pub occurrence_previews: Vec<String>,
	/// Number of fuzzy matches above threshold.
	pub fuzzy_matches:       Option<usize>,
	/// Whether the best fuzzy match was dominant over alternatives.
	pub dominant_fuzzy:      bool,
}

/// Strategy used by [`seek_sequence`] to find a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceMatchStrategy {
	Exact,
	TrimTrailing,
	Trim,
	CommentPrefix,
	Unicode,
	Prefix,
	Substring,
	Fuzzy,
	FuzzyDominant,
	Character,
}

/// Result of a sequence search via [`seek_sequence`].
#[derive(Debug, Clone)]
pub struct SequenceSearchResult {
	/// Line index of the match (0-indexed into the `lines` slice).
	pub index:         Option<usize>,
	/// Confidence score in `[0.0, 1.0]`.
	pub confidence:    f64,
	/// Number of matches found (for ambiguity reporting).
	pub match_count:   Option<usize>,
	/// Indices of up to 5 matches.
	pub match_indices: Vec<usize>,
	/// Which strategy produced the result.
	pub strategy:      Option<SequenceMatchStrategy>,
}

/// Strategy used by [`find_context_line`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMatchStrategy {
	Exact,
	Trim,
	Unicode,
	Prefix,
	Substring,
	Fuzzy,
}

/// Result of a single-line context search via [`find_context_line`].
#[derive(Debug, Clone)]
pub struct ContextLineResult {
	/// Line index of the match (0-indexed).
	pub index:         Option<usize>,
	/// Confidence score in `[0.0, 1.0]`.
	pub confidence:    f64,
	/// Number of matches found.
	pub match_count:   Option<usize>,
	/// Indices of up to 5 matches.
	pub match_indices: Vec<usize>,
	/// Which strategy produced the result.
	pub strategy:      Option<ContextMatchStrategy>,
}

// ───────────────────────────────────────────────────────────────────────────
// Core algorithms
// ───────────────────────────────────────────────────────────────────────────

/// Classic dynamic-programming Levenshtein distance.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
	if a == b {
		return 0;
	}
	let a_bytes = a.as_bytes();
	let b_bytes = b.as_bytes();
	let a_len = a_bytes.len();
	let b_len = b_bytes.len();
	if a_len == 0 {
		return b_len;
	}
	if b_len == 0 {
		return a_len;
	}

	let mut prev: Vec<usize> = (0..=b_len).collect();
	let mut curr = vec![0usize; b_len + 1];

	for i in 1..=a_len {
		curr[0] = i;
		let a_byte = a_bytes[i - 1];
		for j in 1..=b_len {
			let cost = if a_byte == b_bytes[j - 1] { 0 } else { 1 };
			let deletion = prev[j] + 1;
			let insertion = curr[j - 1] + 1;
			let substitution = prev[j - 1] + cost;
			curr[j] = deletion.min(insertion).min(substitution);
		}
		std::mem::swap(&mut prev, &mut curr);
	}

	prev[b_len]
}

/// Similarity score between two strings: `1.0 - distance / max_len`.
///
/// Returns `1.0` for two empty strings.
pub fn similarity(a: &str, b: &str) -> f64 {
	let max_len = a.len().max(b.len());
	if max_len == 0 {
		return 1.0;
	}
	let dist = levenshtein_distance(a, b);
	1.0 - (dist as f64) / (max_len as f64)
}

// ───────────────────────────────────────────────────────────────────────────
// Line-based utilities
// ───────────────────────────────────────────────────────────────────────────

/// Compute byte offsets for each line (assuming lines were split on `\n`).
pub fn compute_line_offsets(lines: &[&str]) -> Vec<usize> {
	let mut offsets = Vec::with_capacity(lines.len());
	let mut offset = 0usize;
	for (i, line) in lines.iter().enumerate() {
		offsets.push(offset);
		offset += line.len();
		if i < lines.len() - 1 {
			offset += 1; // newline character
		}
	}
	offsets
}

/// Compute relative indent depths for a set of lines.
///
/// Non-empty lines get a depth relative to the minimum indentation,
/// measured in indent units (the smallest non-zero indent step).
fn compute_relative_indent_depths(lines: &[&str]) -> Vec<usize> {
	let indents: Vec<usize> = lines.iter().map(|l| count_leading_whitespace(l)).collect();

	let non_empty_indents: Vec<usize> = lines
		.iter()
		.zip(indents.iter())
		.filter(|(l, _)| !l.trim().is_empty())
		.map(|(_, &indent)| indent)
		.collect();

	let min_indent = non_empty_indents.iter().copied().min().unwrap_or(0);

	let indent_unit = non_empty_indents
		.iter()
		.map(|&i| i - min_indent)
		.filter(|&step| step > 0)
		.min()
		.unwrap_or(1)
		.max(1); // avoid division by zero

	lines
		.iter()
		.zip(indents.iter())
		.map(|(line, &indent)| {
			if line.trim().is_empty() {
				return 0;
			}
			let relative = indent.saturating_sub(min_indent);
			// Round to nearest indent unit
			(relative + indent_unit / 2) / indent_unit
		})
		.collect()
}

/// Normalize lines for fuzzy comparison. Each line becomes
/// `"depth|normalized_content"`.
///
/// When `include_depth` is true, the depth prefix is a relative indent level;
/// otherwise it is empty (just `"|"`).
pub fn normalize_lines(lines: &[&str], include_depth: bool) -> Vec<String> {
	let depths = if include_depth {
		Some(compute_relative_indent_depths(lines))
	} else {
		None
	};

	lines
		.iter()
		.enumerate()
		.map(|(i, line)| {
			let trimmed = line.trim();
			let prefix = match &depths {
				Some(d) => format!("{}|", d[i]),
				None => "|".to_owned(),
			};
			if trimmed.is_empty() {
				return prefix;
			}
			format!("{prefix}{}", normalize_for_fuzzy(trimmed))
		})
		.collect()
}

/// Strip common comment prefixes (`//`, `/*`, `*/`, `*`, `#`, `;`) from a line.
pub fn strip_comment_prefix(line: &str) -> String {
	let trimmed = line.trim_start();
	let rest = if trimmed.starts_with("/*") {
		&trimmed[2..]
	} else if trimmed.starts_with("*/") {
		&trimmed[2..]
	} else if trimmed.starts_with("//") {
		&trimmed[2..]
	} else if trimmed.starts_with('*') {
		&trimmed[1..]
	} else if trimmed.starts_with('#') {
		&trimmed[1..]
	} else if trimmed.starts_with(';') {
		&trimmed[1..]
	} else if trimmed.len() >= 2 && trimmed.starts_with('/') && trimmed.as_bytes()[1] == b' ' {
		&trimmed[1..]
	} else {
		trimmed
	};
	rest.trim_start().to_owned()
}

/// Check if `line` starts with `pattern` after fuzzy normalization.
pub fn line_starts_with_pattern(line: &str, pattern: &str) -> bool {
	let line_norm = normalize_for_fuzzy(line);
	let pat_norm = normalize_for_fuzzy(pattern);
	if pat_norm.is_empty() {
		return line_norm.is_empty();
	}
	line_norm.starts_with(&pat_norm)
}

/// Check if `line` contains `pattern` as a significant substring after
/// normalization.
///
/// Returns `false` if the pattern is shorter than [`PARTIAL_MATCH_MIN_LENGTH`]
/// or if its length ratio to the line is below [`PARTIAL_MATCH_MIN_RATIO`].
pub fn line_includes_pattern(line: &str, pattern: &str) -> bool {
	let line_norm = normalize_for_fuzzy(line);
	let pat_norm = normalize_for_fuzzy(pattern);
	if pat_norm.is_empty() {
		return line_norm.is_empty();
	}
	if pat_norm.len() < PARTIAL_MATCH_MIN_LENGTH {
		return false;
	}
	if !line_norm.contains(&pat_norm) {
		return false;
	}
	pat_norm.len() as f64 / line_norm.len().max(1) as f64 >= PARTIAL_MATCH_MIN_RATIO
}

// ───────────────────────────────────────────────────────────────────────────
// Character-level fuzzy match (for replace mode)
// ───────────────────────────────────────────────────────────────────────────

/// Internal result from the sliding-window fuzzy search.
struct BestFuzzyResult {
	best:                  Option<FuzzyMatch>,
	above_threshold_count: usize,
	second_best_score:     f64,
}

/// Sliding window over content lines, scoring each window against the target.
fn find_best_fuzzy_match_core(
	content_lines: &[&str],
	target_lines: &[&str],
	offsets: &[usize],
	threshold: f64,
	include_depth: bool,
) -> BestFuzzyResult {
	let target_normalized = normalize_lines(target_lines, include_depth);

	let mut best: Option<FuzzyMatch> = None;
	let mut best_score: f64 = -1.0;
	let mut second_best_score: f64 = -1.0;
	let mut above_threshold_count: usize = 0;

	let window_count = content_lines.len().saturating_sub(target_lines.len()) + 1;
	for start in 0..window_count {
		let window = &content_lines[start..start + target_lines.len()];
		let window_normalized = normalize_lines(window, include_depth);

		let mut score = 0.0;
		for i in 0..target_lines.len() {
			score += similarity(&target_normalized[i], &window_normalized[i]);
		}
		score /= target_lines.len() as f64;

		if score >= threshold {
			above_threshold_count += 1;
		}

		if score > best_score {
			second_best_score = best_score;
			best_score = score;
			best = Some(FuzzyMatch {
				actual_text: window.join("\n"),
				start_index: offsets[start],
				start_line:  start + 1,
				confidence:  score,
			});
		} else if score > second_best_score {
			second_best_score = score;
		}
	}

	BestFuzzyResult { best, above_threshold_count, second_best_score }
}

/// Find the best fuzzy match for `target` within `content` using a sliding
/// window over lines. Retries without indent depth if the first pass is close
/// but below threshold.
fn find_best_fuzzy_match(content: &str, target: &str, threshold: f64) -> BestFuzzyResult {
	let content_lines: Vec<&str> = content.split('\n').collect();
	let target_lines: Vec<&str> = target.split('\n').collect();

	if target_lines.is_empty() || target.is_empty() {
		return BestFuzzyResult {
			best:                  None,
			above_threshold_count: 0,
			second_best_score:     0.0,
		};
	}
	if target_lines.len() > content_lines.len() {
		return BestFuzzyResult {
			best:                  None,
			above_threshold_count: 0,
			second_best_score:     0.0,
		};
	}

	let offsets = compute_line_offsets(&content_lines);
	let mut result =
		find_best_fuzzy_match_core(&content_lines, &target_lines, &offsets, threshold, true);

	// Retry without indent depth if match is close but below threshold
	if let Some(b) = &result.best
		&& b.confidence < threshold
		&& b.confidence >= FALLBACK_THRESHOLD
	{
		let no_depth =
			find_best_fuzzy_match_core(&content_lines, &target_lines, &offsets, threshold, false);
		if let Some(nd) = &no_depth.best
			&& nd.confidence > b.confidence
		{
			result = no_depth;
		}
	}

	result
}

// ───────────────────────────────────────────────────────────────────────────
// Public: find_match
// ───────────────────────────────────────────────────────────────────────────

/// Find a match for `target` within `content`.
///
/// Tries exact match first. If `allow_fuzzy` is true and no exact match is
/// found, falls back to fuzzy sliding-window matching. Reports ambiguity when
/// multiple occurrences exist.
pub fn find_match(
	content: &str,
	target: &str,
	allow_fuzzy: bool,
	threshold: Option<f64>,
) -> MatchOutcome {
	if target.is_empty() {
		return MatchOutcome::default();
	}

	// Exact match
	if let Some(exact_index) = content.find(target) {
		let occurrences = content.matches(target).count();
		if occurrences > 1 {
			let mut occurrence_lines = Vec::new();
			let mut occurrence_previews = Vec::new();
			let content_lines: Vec<&str> = content.split('\n').collect();
			let mut search_start = 0;
			for _ in 0..MAX_TRACKED_INDICES {
				match content[search_start..].find(target) {
					Some(rel) => {
						let idx = search_start + rel;
						let line_number = content[..idx].split('\n').count();
						occurrence_lines.push(line_number);
						let start = line_number.saturating_sub(1 + OCCURRENCE_PREVIEW_CONTEXT);
						let end = (line_number + OCCURRENCE_PREVIEW_CONTEXT).min(content_lines.len());
						let preview = content_lines[start..end]
							.iter()
							.enumerate()
							.map(|(offset, line)| {
								let num = start + offset + 1;
								let rendered = if line.len() > OCCURRENCE_PREVIEW_MAX_LEN {
									format!("{}…", &line[..OCCURRENCE_PREVIEW_MAX_LEN - 1])
								} else {
									(*line).to_string()
								};
								format!("  {num} | {rendered}")
							})
							.collect::<Vec<_>>()
							.join("\n");
						occurrence_previews.push(preview);
						search_start = idx + 1;
					},
					None => break,
				}
			}
			return MatchOutcome {
				occurrences: Some(occurrences),
				occurrence_lines,
				occurrence_previews,
				..Default::default()
			};
		}
		let start_line = content[..exact_index].split('\n').count();
		return MatchOutcome {
			matched: Some(FuzzyMatch {
				actual_text: target.to_owned(),
				start_index: exact_index,
				start_line,
				confidence: 1.0,
			}),
			..Default::default()
		};
	}

	// Fuzzy match
	let threshold = threshold.unwrap_or(DEFAULT_FUZZY_THRESHOLD);
	let BestFuzzyResult { best, above_threshold_count, second_best_score } =
		find_best_fuzzy_match(content, target, threshold);

	let best = match best {
		Some(b) => b,
		None => return MatchOutcome::default(),
	};

	if allow_fuzzy && best.confidence >= threshold {
		if above_threshold_count == 1 {
			return MatchOutcome {
				matched: Some(best.clone()),
				closest: Some(best),
				..Default::default()
			};
		}
		if above_threshold_count > 1
			&& best.confidence >= DOMINANT_MIN
			&& best.confidence - second_best_score >= DOMINANT_DELTA
		{
			return MatchOutcome {
				matched: Some(best.clone()),
				closest: Some(best),
				fuzzy_matches: Some(above_threshold_count),
				dominant_fuzzy: true,
				..Default::default()
			};
		}
	}

	MatchOutcome {
		closest: Some(best),
		fuzzy_matches: Some(above_threshold_count),
		..Default::default()
	}
}

// ───────────────────────────────────────────────────────────────────────────
// Sequence matching helpers
// ───────────────────────────────────────────────────────────────────────────

/// Check if `pattern` matches `lines` starting at index `i` using `compare`.
fn matches_at<F>(lines: &[&str], pattern: &[&str], i: usize, compare: F) -> bool
where
	F: Fn(&str, &str) -> bool,
{
	for j in 0..pattern.len() {
		if !compare(lines[i + j], pattern[j]) {
			return false;
		}
	}
	true
}

/// Average similarity score for `pattern` at position `i`, after fuzzy
/// normalization.
fn fuzzy_score_at(lines: &[&str], pattern: &[&str], i: usize) -> f64 {
	let mut total = 0.0;
	for j in 0..pattern.len() {
		let line_norm = normalize_for_fuzzy(lines[i + j]);
		let pat_norm = normalize_for_fuzzy(pattern[j]);
		total += similarity(&line_norm, &pat_norm);
	}
	total / pattern.len() as f64
}

/// Result from the deterministic (non-fuzzy) passes in `seek_sequence`.
struct ExactPassResult {
	result: SequenceSearchResult,
}

/// Run passes 1–6 (exact through substring) over the range `[from, to]`.
///
/// Returns `None` if no pass matched.
fn run_exact_passes(
	lines: &[&str],
	pattern: &[&str],
	from: usize,
	to: usize,
	allow_fuzzy: bool,
) -> Option<ExactPassResult> {
	// Pass 1: Exact
	for i in from..=to {
		if matches_at(lines, pattern, i, |a, b| a == b) {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index:         Some(i),
					confidence:    1.0,
					match_count:   None,
					match_indices: vec![],
					strategy:      Some(SequenceMatchStrategy::Exact),
				},
			});
		}
	}

	// Pass 2: Trailing whitespace stripped
	for i in from..=to {
		if matches_at(lines, pattern, i, |a, b| a.trim_end() == b.trim_end()) {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index:         Some(i),
					confidence:    0.99,
					match_count:   None,
					match_indices: vec![],
					strategy:      Some(SequenceMatchStrategy::TrimTrailing),
				},
			});
		}
	}

	// Pass 3: Full trim
	for i in from..=to {
		if matches_at(lines, pattern, i, |a, b| a.trim() == b.trim()) {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index:         Some(i),
					confidence:    0.98,
					match_count:   None,
					match_indices: vec![],
					strategy:      Some(SequenceMatchStrategy::Trim),
				},
			});
		}
	}

	// Pass 3b: Comment-prefix normalized
	for i in from..=to {
		if matches_at(lines, pattern, i, |a, b| strip_comment_prefix(a) == strip_comment_prefix(b)) {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index:         Some(i),
					confidence:    0.975,
					match_count:   None,
					match_indices: vec![],
					strategy:      Some(SequenceMatchStrategy::CommentPrefix),
				},
			});
		}
	}

	// Pass 4: Unicode normalization
	for i in from..=to {
		if matches_at(lines, pattern, i, |a, b| normalize_unicode(a) == normalize_unicode(b)) {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index:         Some(i),
					confidence:    0.97,
					match_count:   None,
					match_indices: vec![],
					strategy:      Some(SequenceMatchStrategy::Unicode),
				},
			});
		}
	}

	if !allow_fuzzy {
		return None;
	}

	// Pass 5: Prefix match
	{
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in from..=to {
			if matches_at(lines, pattern, i, line_starts_with_pattern) {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index: first_match,
					confidence: 0.965,
					match_count: Some(match_count),
					match_indices,
					strategy: Some(SequenceMatchStrategy::Prefix),
				},
			});
		}
	}

	// Pass 6: Substring match
	{
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in from..=to {
			if matches_at(lines, pattern, i, line_includes_pattern) {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return Some(ExactPassResult {
				result: SequenceSearchResult {
					index: first_match,
					confidence: 0.94,
					match_count: Some(match_count),
					match_indices,
					strategy: Some(SequenceMatchStrategy::Substring),
				},
			});
		}
	}

	None
}

// ───────────────────────────────────────────────────────────────────────────
// Public: seek_sequence
// ───────────────────────────────────────────────────────────────────────────

/// Find a sequence of `pattern` lines within `lines`.
///
/// Attempts matches with decreasing strictness across 8 passes:
/// 1. Exact match
/// 2. Trailing whitespace ignored
/// 3. All whitespace trimmed
/// 4. Comment prefix normalized
/// 5. Unicode punctuation normalized
/// 6. Prefix match (pattern is prefix of line)
/// 7. Substring match (pattern is significant substring)
/// 8. Fuzzy similarity match
/// 9. Character-based fallback via [`find_match`]
///
/// When `eof` is true, the search prefers matches near the end of the file.
pub fn seek_sequence(
	lines: &[&str],
	pattern: &[&str],
	start: usize,
	eof: bool,
	allow_fuzzy: bool,
) -> SequenceSearchResult {
	// Empty pattern matches immediately
	if pattern.is_empty() {
		return SequenceSearchResult {
			index:         Some(start),
			confidence:    1.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      Some(SequenceMatchStrategy::Exact),
		};
	}

	// Pattern longer than available content cannot match
	if pattern.len() > lines.len() {
		return SequenceSearchResult {
			index:         None,
			confidence:    0.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      None,
		};
	}

	let max_start = lines.len() - pattern.len();
	let search_start = if eof && lines.len() >= pattern.len() {
		max_start
	} else {
		start
	};

	// Primary deterministic passes
	if let Some(ep) = run_exact_passes(lines, pattern, search_start, max_start, allow_fuzzy) {
		return ep.result;
	}

	// If eof mode started from end, also try from the original start
	if eof
		&& search_start > start
		&& let Some(ep) = run_exact_passes(lines, pattern, start, max_start, allow_fuzzy)
	{
		return ep.result;
	}

	if !allow_fuzzy {
		return SequenceSearchResult {
			index:         None,
			confidence:    0.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      None,
		};
	}

	// Pass 7: Fuzzy matching — find best above threshold
	let mut best_index: Option<usize> = None;
	let mut best_score: f64 = 0.0;
	let mut second_best_score: f64 = 0.0;
	let mut match_count: usize = 0;
	let mut match_indices: Vec<usize> = Vec::new();

	let consider = |i: usize,
	                best_index: &mut Option<usize>,
	                best_score: &mut f64,
	                second_best_score: &mut f64,
	                match_count: &mut usize,
	                match_indices: &mut Vec<usize>| {
		let score = fuzzy_score_at(lines, pattern, i);
		if score >= SEQUENCE_FUZZY_THRESHOLD {
			*match_count += 1;
			if match_indices.len() < MAX_TRACKED_INDICES {
				match_indices.push(i);
			}
		}
		if score > *best_score {
			*second_best_score = *best_score;
			*best_score = score;
			*best_index = Some(i);
		} else if score > *second_best_score {
			*second_best_score = score;
		}
	};

	for i in search_start..=max_start {
		consider(
			i,
			&mut best_index,
			&mut best_score,
			&mut second_best_score,
			&mut match_count,
			&mut match_indices,
		);
	}

	// Also search from start if eof mode started from end
	if eof && search_start > start {
		for i in start..search_start {
			consider(
				i,
				&mut best_index,
				&mut best_score,
				&mut second_best_score,
				&mut match_count,
				&mut match_indices,
			);
		}
	}

	if let Some(bi) = best_index
		&& best_score >= SEQUENCE_FUZZY_THRESHOLD
	{
		// Dominant fuzzy: single clear winner among multiple matches
		if match_count > 1
			&& best_score >= DOMINANT_MIN
			&& best_score - second_best_score >= DOMINANT_DELTA
		{
			return SequenceSearchResult {
				index: Some(bi),
				confidence: best_score,
				match_count: Some(1),
				match_indices,
				strategy: Some(SequenceMatchStrategy::FuzzyDominant),
			};
		}
		return SequenceSearchResult {
			index: Some(bi),
			confidence: best_score,
			match_count: Some(match_count),
			match_indices,
			strategy: Some(SequenceMatchStrategy::Fuzzy),
		};
	}

	// Pass 8: Character-based fallback via find_match
	let pattern_text = pattern.join("\n");
	let content_text = lines[start..].join("\n");
	let outcome = find_match(&content_text, &pattern_text, true, Some(CHARACTER_MATCH_THRESHOLD));

	if let Some(m) = &outcome.matched {
		// Convert character index back to line index
		let matched_prefix = &content_text[..m.start_index];
		let line_index = start + matched_prefix.split('\n').count() - 1;
		let fallback_count = outcome.occurrences.or(outcome.fuzzy_matches).unwrap_or(1);
		return SequenceSearchResult {
			index:         Some(line_index),
			confidence:    m.confidence,
			match_count:   Some(fallback_count),
			match_indices: vec![],
			strategy:      Some(SequenceMatchStrategy::Character),
		};
	}

	let fallback_count = outcome.occurrences.or(outcome.fuzzy_matches);
	SequenceSearchResult {
		index:         None,
		confidence:    best_score,
		match_count:   fallback_count,
		match_indices: vec![],
		strategy:      None,
	}
}

// ───────────────────────────────────────────────────────────────────────────
// Public: find_closest_sequence_match
// ───────────────────────────────────────────────────────────────────────────

/// Find the closest fuzzy match for `pattern` in `lines`, ignoring ambiguity.
///
/// Always uses fuzzy scoring — no deterministic passes. Returns the single
/// best-scoring position.
pub fn find_closest_sequence_match(
	lines: &[&str],
	pattern: &[&str],
	start: Option<usize>,
	eof: bool,
) -> SequenceSearchResult {
	let start = start.unwrap_or(0);

	if pattern.is_empty() {
		return SequenceSearchResult {
			index:         Some(start),
			confidence:    1.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      Some(SequenceMatchStrategy::Exact),
		};
	}
	if pattern.len() > lines.len() {
		return SequenceSearchResult {
			index:         None,
			confidence:    0.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      Some(SequenceMatchStrategy::Fuzzy),
		};
	}

	let max_start = lines.len() - pattern.len();
	let search_start = if eof && lines.len() >= pattern.len() {
		max_start
	} else {
		start
	};

	let mut best_index: Option<usize> = None;
	let mut best_score: f64 = 0.0;

	for i in search_start..=max_start {
		let score = fuzzy_score_at(lines, pattern, i);
		if score > best_score {
			best_score = score;
			best_index = Some(i);
		}
	}

	if eof && search_start > start {
		for i in start..search_start {
			let score = fuzzy_score_at(lines, pattern, i);
			if score > best_score {
				best_score = score;
				best_index = Some(i);
			}
		}
	}

	SequenceSearchResult {
		index:         best_index,
		confidence:    best_score,
		match_count:   None,
		match_indices: vec![],
		strategy:      Some(SequenceMatchStrategy::Fuzzy),
	}
}

// ───────────────────────────────────────────────────────────────────────────
// Public: find_context_line
// ───────────────────────────────────────────────────────────────────────────

/// Find a single context line in `lines` using progressive matching strategies.
///
/// Passes:
/// 1. Exact line match
/// 2. Trimmed match
/// 3. Unicode normalization
/// 4. Prefix match
/// 5. Substring match (with ratio filtering for ambiguity)
/// 6. Fuzzy similarity
///
/// If the context ends with `()`, an extra fallback retries with `(` and
/// without parens.
pub fn find_context_line(
	lines: &[&str],
	context: &str,
	start: usize,
	allow_fuzzy: bool,
) -> ContextLineResult {
	find_context_line_inner(lines, context, start, allow_fuzzy, false)
}

fn find_context_line_inner(
	lines: &[&str],
	context: &str,
	start: usize,
	allow_fuzzy: bool,
	skip_function_fallback: bool,
) -> ContextLineResult {
	let trimmed_context = context.trim();

	// Pass 1: Exact
	{
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in start..lines.len() {
			if lines[i] == context {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return ContextLineResult {
				index: first_match,
				confidence: 1.0,
				match_count: Some(match_count),
				match_indices,
				strategy: Some(ContextMatchStrategy::Exact),
			};
		}
	}

	// Pass 2: Trimmed
	{
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in start..lines.len() {
			if lines[i].trim() == trimmed_context {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return ContextLineResult {
				index: first_match,
				confidence: 0.99,
				match_count: Some(match_count),
				match_indices,
				strategy: Some(ContextMatchStrategy::Trim),
			};
		}
	}

	// Pass 3: Unicode normalization
	let normalized_context = normalize_unicode(context);
	{
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in start..lines.len() {
			if normalize_unicode(lines[i]) == normalized_context {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return ContextLineResult {
				index: first_match,
				confidence: 0.98,
				match_count: Some(match_count),
				match_indices,
				strategy: Some(ContextMatchStrategy::Unicode),
			};
		}
	}

	if !allow_fuzzy {
		return ContextLineResult {
			index:         None,
			confidence:    0.0,
			match_count:   None,
			match_indices: vec![],
			strategy:      None,
		};
	}

	// Pass 4: Prefix match
	let context_norm = normalize_for_fuzzy(context);
	if !context_norm.is_empty() {
		let mut first_match = None;
		let mut match_count = 0usize;
		let mut match_indices = Vec::new();
		for i in start..lines.len() {
			let line_norm = normalize_for_fuzzy(lines[i]);
			if line_norm.starts_with(&context_norm) {
				if first_match.is_none() {
					first_match = Some(i);
				}
				match_count += 1;
				if match_indices.len() < MAX_TRACKED_INDICES {
					match_indices.push(i);
				}
			}
		}
		if match_count > 0 {
			return ContextLineResult {
				index: first_match,
				confidence: 0.96,
				match_count: Some(match_count),
				match_indices,
				strategy: Some(ContextMatchStrategy::Prefix),
			};
		}
	}

	// Pass 5: Substring match
	if context_norm.len() >= PARTIAL_MATCH_MIN_LENGTH {
		let mut all_substring_matches: Vec<(usize, f64)> = Vec::new();
		for i in start..lines.len() {
			let line_norm = normalize_for_fuzzy(lines[i]);
			if line_norm.contains(&context_norm) {
				let ratio = context_norm.len() as f64 / line_norm.len().max(1) as f64;
				all_substring_matches.push((i, ratio));
			}
		}

		let match_indices: Vec<usize> = all_substring_matches
			.iter()
			.take(MAX_TRACKED_INDICES)
			.map(|&(idx, _)| idx)
			.collect();

		// If exactly one substring match, accept regardless of ratio
		if all_substring_matches.len() == 1 {
			return ContextLineResult {
				index: Some(all_substring_matches[0].0),
				confidence: 0.94,
				match_count: Some(1),
				match_indices,
				strategy: Some(ContextMatchStrategy::Substring),
			};
		}

		// Multiple matches: filter by ratio
		if !all_substring_matches.is_empty() {
			let mut first_match = None;
			let mut match_count = 0usize;
			for &(idx, ratio) in &all_substring_matches {
				if ratio >= PARTIAL_MATCH_MIN_RATIO {
					if first_match.is_none() {
						first_match = Some(idx);
					}
					match_count += 1;
				}
			}
			if match_count > 0 {
				return ContextLineResult {
					index: first_match,
					confidence: 0.94,
					match_count: Some(match_count),
					match_indices,
					strategy: Some(ContextMatchStrategy::Substring),
				};
			}

			// Had substring matches but none passed ratio — return ambiguous
			if all_substring_matches.len() > 1 {
				return ContextLineResult {
					index: Some(all_substring_matches[0].0),
					confidence: 0.94,
					match_count: Some(all_substring_matches.len()),
					match_indices,
					strategy: Some(ContextMatchStrategy::Substring),
				};
			}
		}
	}

	// Pass 6: Fuzzy similarity
	let mut best_index: Option<usize> = None;
	let mut best_score: f64 = 0.0;
	let mut match_count = 0usize;
	let mut match_indices: Vec<usize> = Vec::new();

	for i in start..lines.len() {
		let line_norm = normalize_for_fuzzy(lines[i]);
		let score = similarity(&line_norm, &context_norm);
		if score >= CONTEXT_FUZZY_THRESHOLD {
			match_count += 1;
			if match_indices.len() < MAX_TRACKED_INDICES {
				match_indices.push(i);
			}
		}
		if score > best_score {
			best_score = score;
			best_index = Some(i);
		}
	}

	if best_index.is_some() && best_score >= CONTEXT_FUZZY_THRESHOLD {
		return ContextLineResult {
			index: best_index,
			confidence: best_score,
			match_count: Some(match_count),
			match_indices,
			strategy: Some(ContextMatchStrategy::Fuzzy),
		};
	}

	// Function call fallback: if context ends with "()", retry with "(" and without
	if !skip_function_fallback && trimmed_context.ends_with("()") {
		let base = trimmed_context.trim_end_matches("()").trim_end();
		let with_paren = format!("{base}(");
		let result = find_context_line_inner(lines, &with_paren, start, allow_fuzzy, true);
		if result.index.is_some() || result.match_count.is_some_and(|c| c > 0) {
			return result;
		}
		return find_context_line_inner(lines, base, start, allow_fuzzy, true);
	}

	ContextLineResult {
		index:         None,
		confidence:    best_score,
		match_count:   None,
		match_indices: vec![],
		strategy:      None,
	}
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// -- levenshtein_distance --

	#[test]
	fn levenshtein_identical() {
		assert_eq!(levenshtein_distance("abc", "abc"), 0);
	}

	#[test]
	fn levenshtein_empty_strings() {
		assert_eq!(levenshtein_distance("", ""), 0);
		assert_eq!(levenshtein_distance("abc", ""), 3);
		assert_eq!(levenshtein_distance("", "xyz"), 3);
	}

	#[test]
	fn levenshtein_single_edit() {
		assert_eq!(levenshtein_distance("kitten", "sitten"), 1);
		assert_eq!(levenshtein_distance("kitten", "kittens"), 1);
		assert_eq!(levenshtein_distance("kitten", "kiten"), 1);
	}

	#[test]
	fn levenshtein_multi_edit() {
		assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
	}

	// -- similarity --

	#[test]
	fn similarity_identical() {
		assert!((similarity("hello", "hello") - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn similarity_empty() {
		assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn similarity_completely_different() {
		let s = similarity("abc", "xyz");
		assert!(s < 0.01, "expected near 0, got {s}");
	}

	#[test]
	fn similarity_partial() {
		let s = similarity("hello", "helo");
		// distance 1, max_len 5 => 0.8
		assert!((s - 0.8).abs() < f64::EPSILON);
	}

	// -- find_match --

	#[test]
	fn find_match_empty_target() {
		let r = find_match("some content", "", false, None);
		assert!(r.matched.is_none());
		assert!(r.closest.is_none());
	}

	#[test]
	fn find_match_exact_single() {
		let content = "line one\nline two\nline three";
		let r = find_match(content, "line two", false, None);
		let m = r.matched.expect("should find exact match");
		assert_eq!(m.actual_text, "line two");
		assert_eq!(m.start_line, 2);
		assert!((m.confidence - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn find_match_exact_multiple_occurrences() {
		let content = "foo\nbar\nfoo\nbaz\nfoo";
		let r = find_match(content, "foo", false, None);
		assert!(r.matched.is_none(), "ambiguous: should not return a match");
		assert_eq!(r.occurrences, Some(3));
		assert_eq!(r.occurrence_lines.len(), 3);
		assert_eq!(r.occurrence_lines, vec![1, 3, 5]);
	}

	#[test]
	fn find_match_fuzzy_allowed() {
		// Content with a slight typo vs the target
		let content = "fn process_data() {\n    let result = compute();\n    return result;\n}";
		let target = "fn process_data() {\n    let rsult = compute();\n    return result;\n}";
		let r = find_match(content, target, true, Some(0.9));
		assert!(r.matched.is_some() || r.closest.is_some());
	}

	#[test]
	fn find_match_fuzzy_not_allowed() {
		let content = "fn process_data() {\n    let result = compute();\n    return result;\n}";
		let target = "fn process_data() {\n    let rsult = compute();\n    return result;\n}";
		let r = find_match(content, target, false, None);
		// Not an exact match, fuzzy not allowed → no matched
		assert!(r.matched.is_none());
	}

	// -- seek_sequence --

	#[test]
	fn seek_sequence_empty_pattern() {
		let lines = vec!["a", "b", "c"];
		let r = seek_sequence(&lines, &[], 0, false, true);
		assert_eq!(r.index, Some(0));
		assert!((r.confidence - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn seek_sequence_exact() {
		let lines = vec!["alpha", "beta", "gamma", "delta"];
		let pattern = vec!["beta", "gamma"];
		let r = seek_sequence(&lines, &pattern, 0, false, true);
		assert_eq!(r.index, Some(1));
		assert!((r.confidence - 1.0).abs() < f64::EPSILON);
		assert_eq!(r.strategy, Some(SequenceMatchStrategy::Exact));
	}

	#[test]
	fn seek_sequence_trim() {
		let lines = vec!["  alpha  ", "  beta  ", "  gamma  "];
		let pattern = vec!["alpha", "beta"];
		let r = seek_sequence(&lines, &pattern, 0, false, true);
		assert_eq!(r.index, Some(0));
		assert!(r.confidence >= 0.98);
		assert_eq!(r.strategy, Some(SequenceMatchStrategy::Trim));
	}

	#[test]
	fn seek_sequence_pattern_too_long() {
		let lines = vec!["a"];
		let pattern = vec!["a", "b", "c"];
		let r = seek_sequence(&lines, &pattern, 0, false, true);
		assert!(r.index.is_none());
	}

	#[test]
	fn seek_sequence_eof_prefers_end() {
		let lines = vec!["x", "y", "x", "y"];
		let pattern = vec!["x", "y"];
		// With eof=true, search starts from the end
		let r = seek_sequence(&lines, &pattern, 0, true, true);
		assert_eq!(r.index, Some(2));
		assert_eq!(r.strategy, Some(SequenceMatchStrategy::Exact));
	}

	#[test]
	fn seek_sequence_fuzzy_fallback() {
		let lines = vec!["fn main() {", "    println!(\"hello world\");", "}"];
		// Slight typo: "helo" instead of "hello"
		let pattern = vec!["fn main() {", "    println!(\"helo world\");", "}"];
		let r = seek_sequence(&lines, &pattern, 0, false, true);
		assert_eq!(r.index, Some(0));
		assert!(r.confidence > 0.9);
	}

	// -- find_context_line --

	#[test]
	fn context_line_exact() {
		let lines = vec!["foo", "bar", "baz"];
		let r = find_context_line(&lines, "bar", 0, true);
		assert_eq!(r.index, Some(1));
		assert!((r.confidence - 1.0).abs() < f64::EPSILON);
		assert_eq!(r.strategy, Some(ContextMatchStrategy::Exact));
	}

	#[test]
	fn context_line_trimmed() {
		let lines = vec!["  foo  ", "  bar  ", "  baz  "];
		let r = find_context_line(&lines, "bar", 0, true);
		assert_eq!(r.index, Some(1));
		assert_eq!(r.strategy, Some(ContextMatchStrategy::Trim));
	}

	#[test]
	fn context_line_not_found() {
		let lines = vec!["foo", "bar", "baz"];
		let r = find_context_line(&lines, "completely_different_xxxxxx", 0, true);
		assert!(r.index.is_none());
	}

	#[test]
	fn context_line_function_fallback() {
		let lines = vec!["fn process(data: &str) {", "    // body", "}"];
		// Context ends with "()" — should retry with "(" and without
		let r = find_context_line(&lines, "fn process()", 0, true);
		// Should match via prefix or substring fallback
		assert!(r.index.is_some());
	}

	// -- strip_comment_prefix --

	#[test]
	fn strip_comment_various() {
		assert_eq!(strip_comment_prefix("// hello"), "hello");
		assert_eq!(strip_comment_prefix("# comment"), "comment");
		assert_eq!(strip_comment_prefix("/* block */"), "block */");
		assert_eq!(strip_comment_prefix("  * item"), "item");
		assert_eq!(strip_comment_prefix("; lisp"), "lisp");
	}

	// -- line_starts_with_pattern --

	#[test]
	fn starts_with_pattern() {
		assert!(line_starts_with_pattern("  hello world  ", "hello"));
		assert!(!line_starts_with_pattern("  world hello  ", "hello"));
	}

	// -- line_includes_pattern --

	#[test]
	fn includes_pattern_significant() {
		// Pattern "process_data" is 12 chars, well above minimum 6
		assert!(line_includes_pattern("fn process_data(x: i32) -> bool {", "process_data"));
	}

	#[test]
	fn includes_pattern_too_short() {
		// Pattern "fn" is only 2 chars, below minimum 6
		assert!(!line_includes_pattern("fn main() {}", "fn"));
	}

	// -- compute_line_offsets --

	#[test]
	fn line_offsets() {
		let lines = vec!["abc", "de", "f"];
		let offsets = compute_line_offsets(&lines);
		// "abc" at 0, "de" at 4 (3 + newline), "f" at 7 (4+2+1)
		assert_eq!(offsets, vec![0, 4, 7]);
	}

	// -- find_closest_sequence_match --

	#[test]
	fn closest_sequence_empty_pattern() {
		let lines = vec!["a", "b"];
		let r = find_closest_sequence_match(&lines, &[], None, false);
		assert_eq!(r.index, Some(0));
		assert!((r.confidence - 1.0).abs() < f64::EPSILON);
	}

	#[test]
	fn closest_sequence_basic() {
		let lines = vec!["alpha", "beta", "gamma"];
		let pattern = vec!["bta", "gama"]; // typos
		let r = find_closest_sequence_match(&lines, &pattern, None, false);
		assert_eq!(r.index, Some(1));
		assert!(r.confidence > 0.5);
	}
}
