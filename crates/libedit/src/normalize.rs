//! Text normalization utilities for the edit tool.
//!
//! Handles line endings, BOM, whitespace, indentation, and Unicode
//! normalization.

// ─── Line Ending ─────────────────────────────────────────────────────────────

/// Detected line-ending style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
	Lf,
	Crlf,
}

/// Detect the predominant line ending in `content`.
///
/// Returns [`LineEnding::Crlf`] only when the first `\r\n` appears at or before
/// the first bare `\n`. Defaults to [`LineEnding::Lf`].
pub fn detect_line_ending(content: &str) -> LineEnding {
	let crlf_idx = content.find("\r\n");
	let lf_idx = content.find('\n');
	match (crlf_idx, lf_idx) {
		(Some(c), Some(l)) if c <= l => LineEnding::Crlf,
		_ => LineEnding::Lf,
	}
}

/// Replace `\r\n` and lone `\r` with `\n`.
pub fn normalize_to_lf(text: &str) -> String {
	// Two-pass: CRLF first so lone CR is handled second.
	text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore line endings to `ending`. If `Lf`, the text is returned unchanged.
pub fn restore_line_endings(text: &str, ending: LineEnding) -> String {
	match ending {
		LineEnding::Lf => text.to_owned(),
		LineEnding::Crlf => text.replace('\n', "\r\n"),
	}
}

// ─── BOM ─────────────────────────────────────────────────────────────────────

/// Result of [`strip_bom`]: the BOM prefix (if any) and the remaining text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BomResult<'a> {
	/// The BOM string (`"\u{FEFF}"`) if present, otherwise `""`.
	pub bom:  &'a str,
	/// The text after stripping the BOM.
	pub text: &'a str,
}

/// Strip a leading UTF-8 BOM (`U+FEFF`) if present.
pub fn strip_bom(content: &str) -> BomResult<'_> {
	if let Some(rest) = content.strip_prefix('\u{FEFF}') {
		BomResult { bom: &content[..'\u{FEFF}'.len_utf8()], text: rest }
	} else {
		BomResult { bom: "", text: content }
	}
}

// ─── Whitespace ──────────────────────────────────────────────────────────────

/// Count leading spaces and tabs in `line`.
pub fn count_leading_whitespace(line: &str) -> usize {
	line.chars().take_while(|&c| c == ' ' || c == '\t').count()
}

/// Return the leading whitespace slice of `line`.
pub fn get_leading_whitespace(line: &str) -> &str {
	let end = line
		.as_bytes()
		.iter()
		.position(|&b| b != b' ' && b != b'\t')
		.unwrap_or(line.len());
	&line[..end]
}

/// Minimum indentation (in characters) of non-empty lines.
///
/// Returns `0` if the text is empty or all lines are blank.
pub fn min_indent(text: &str) -> usize {
	text
		.split('\n')
		.filter(|l| l.chars().any(|c| !c.is_whitespace()))
		.map(count_leading_whitespace)
		.min()
		.unwrap_or(0)
}

/// Detect the indentation character used in `text`.
///
/// Returns the first character of the first non-empty indentation found,
/// defaulting to `' '` (space).
pub fn detect_indent_char(text: &str) -> char {
	for line in text.split('\n') {
		let ws = get_leading_whitespace(line);
		if !ws.is_empty() {
			// Safe: ws is non-empty ASCII.
			return ws.as_bytes()[0] as char;
		}
	}
	' '
}

/// Replace leading tabs with spaces (`spaces_per_tab` spaces per tab).
///
/// Only converts lines whose leading whitespace is purely tabs (no mixed).
pub fn convert_leading_tabs_to_spaces(text: &str, spaces_per_tab: usize) -> String {
	if spaces_per_tab == 0 {
		return text.to_owned();
	}
	text
		.split('\n')
		.map(|line| {
			let trimmed = line.trim_start();
			if trimmed.is_empty() {
				return line.to_owned();
			}
			let leading = get_leading_whitespace(line);
			// Only convert if leading is purely tabs (no spaces mixed in).
			if !leading.contains('\t') || leading.contains(' ') {
				return line.to_owned();
			}
			let spaces = " ".repeat(leading.len() * spaces_per_tab);
			format!("{spaces}{trimmed}")
		})
		.collect::<Vec<_>>()
		.join("\n")
}

// ─── Unicode Normalization ───────────────────────────────────────────────────

/// Replace fancy Unicode punctuation with ASCII equivalents, strip zero-width
/// characters, and apply NFC-like normalization (manual replacement table — no
/// ICU dependency).
pub fn normalize_unicode(s: &str) -> String {
	let mut result = String::with_capacity(s.len());
	for ch in s.trim().chars() {
		match ch {
			// Dashes / hyphens → '-'
			'\u{2010}'..='\u{2015}' | '\u{2212}' => result.push('-'),
			// Fancy single quotes → '
			'\u{2018}'..='\u{201B}' => result.push('\''),
			// Fancy double quotes → "
			'\u{201C}'..='\u{201F}' => result.push('"'),
			// Odd spaces → normal space
			'\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => {
				result.push(' ');
			},
			// Not-equal sign → !=
			'\u{2260}' => result.push_str("!="),
			// Vulgar fraction ½ → 1/2
			'\u{00BD}' => result.push_str("1/2"),
			// Zero-width characters → remove
			'\u{200B}'..='\u{200D}' | '\u{FEFF}' => {},
			other => result.push(other),
		}
	}
	result
}

/// Normalize a line for fuzzy comparison.
///
/// Trims whitespace, collapses interior runs of spaces/tabs to a single space,
/// and normalizes fancy quotes and dashes to ASCII.
pub fn normalize_for_fuzzy(line: &str) -> String {
	let trimmed = line.trim();
	if trimmed.is_empty() {
		return String::new();
	}

	let mut out = String::with_capacity(trimmed.len());
	let mut prev_space = false;
	for ch in trimmed.chars() {
		let replacement = match ch {
			// Fancy double quotes
			'\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{00AB}' | '\u{00BB}' => '"',
			// Fancy single quotes
			'\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '`' | '\u{00B4}' => '\'',
			// Dashes
			'\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
			// Whitespace collapse
			' ' | '\t' => {
				if !prev_space {
					prev_space = true;
					out.push(' ');
				}
				continue;
			},
			other => other,
		};
		prev_space = false;
		out.push(replacement);
	}
	out
}

// ─── Indentation Adjustment ──────────────────────────────────────────────────

/// Internal indent profile for a block of text.
#[allow(dead_code)]
struct IndentProfile {
	lines:           Vec<String>,
	indent_counts:   Vec<usize>,
	min:             usize,
	char:            Option<char>,
	space_only:      bool,
	tab_only:        bool,
	mixed:           bool,
	unit:            usize,
	non_empty_count: usize,
}

fn gcd(a: usize, b: usize) -> usize {
	let (mut x, mut y) = (a, b);
	while y != 0 {
		let t = y;
		y = x % y;
		x = t;
	}
	x
}

fn build_indent_profile(text: &str) -> IndentProfile {
	let lines: Vec<String> = text.split('\n').map(String::from).collect();
	let mut indent_counts: Vec<usize> = Vec::new();
	let mut min = usize::MAX;
	let mut indent_char: Option<char> = None;
	let mut space_only = true;
	let mut tab_only = true;
	let mut mixed = false;
	let mut non_empty_count: usize = 0;
	let mut unit: usize = 0;

	for line in &lines {
		if line.trim().is_empty() {
			continue;
		}
		non_empty_count += 1;
		let indent = get_leading_whitespace(line);
		indent_counts.push(indent.len());
		min = min.min(indent.len());
		if indent.contains(' ') {
			tab_only = false;
		}
		if indent.contains('\t') {
			space_only = false;
		}
		if indent.contains(' ') && indent.contains('\t') {
			mixed = true;
		}
		if !indent.is_empty() {
			let current = indent.as_bytes()[0] as char;
			match indent_char {
				None => indent_char = Some(current),
				Some(existing) if existing != current => mixed = true,
				_ => {},
			}
		}
	}

	if min == usize::MAX {
		min = 0;
	}

	if space_only && non_empty_count > 0 {
		let mut current: usize = 0;
		for &count in &indent_counts {
			if count == 0 {
				continue;
			}
			current = if current == 0 {
				count
			} else {
				gcd(current, count)
			};
		}
		unit = current;
	}

	if tab_only && non_empty_count > 0 {
		unit = 1;
	}

	IndentProfile {
		lines,
		indent_counts,
		min,
		char: indent_char,
		space_only,
		tab_only,
		mixed,
		unit,
		non_empty_count,
	}
}

/// Adjust `new_text` indentation to match the delta between `old_text` and
/// `actual_text`.
///
/// When the agent provides `old_text` at one indentation level but the file
/// actually has `actual_text` at a different level, this function re-indents
/// `new_text` by the detected delta so the replacement aligns with the
/// surrounding code.
///
/// Returns `new_text` unchanged when:
/// - `old_text == actual_text`
/// - The change is purely an indentation change (same trimmed content)
/// - Any text has mixed indent characters
/// - Deltas are inconsistent across lines
/// - Any profile has no non-empty lines
pub fn adjust_indentation(old_text: &str, actual_text: &str, new_text: &str) -> String {
	// Exact match — no adjustment needed.
	if old_text == actual_text {
		return new_text.to_owned();
	}

	// If the patch is purely an indentation change, return as-is.
	let old_lines: Vec<&str> = old_text.split('\n').collect();
	let new_lines: Vec<&str> = new_text.split('\n').collect();
	if old_lines.len() == new_lines.len() {
		let indentation_only = old_lines
			.iter()
			.zip(new_lines.iter())
			.all(|(o, n)| o.trim() == n.trim());
		if indentation_only {
			return new_text.to_owned();
		}
	}

	let old_profile = build_indent_profile(old_text);
	let actual_profile = build_indent_profile(actual_text);
	let new_profile = build_indent_profile(new_text);

	if new_profile.non_empty_count == 0
		|| old_profile.non_empty_count == 0
		|| actual_profile.non_empty_count == 0
	{
		return new_text.to_owned();
	}

	if old_profile.mixed || actual_profile.mixed || new_profile.mixed {
		return new_text.to_owned();
	}

	// Handle tab↔space conversion.
	if let (Some(old_ch), Some(actual_ch)) = (old_profile.char, actual_profile.char)
		&& old_ch != actual_ch
	{
		if actual_profile.space_only
			&& old_profile.tab_only
			&& new_profile.tab_only
			&& actual_profile.unit > 0
		{
			// Check consistency: each old tab-indent maps to actual spaces
			// at `actual_profile.unit` spaces per tab.
			let line_count = old_profile.lines.len().min(actual_profile.lines.len());
			let mut consistent = true;
			for i in 0..line_count {
				let old_line = &old_profile.lines[i];
				let actual_line = &actual_profile.lines[i];
				if old_line.trim().is_empty() || actual_line.trim().is_empty() {
					continue;
				}
				let old_indent = get_leading_whitespace(old_line);
				if old_indent.is_empty() {
					continue;
				}
				let actual_indent = get_leading_whitespace(actual_line);
				if actual_indent.len() != old_indent.len() * actual_profile.unit {
					consistent = false;
					break;
				}
			}
			return if consistent {
				convert_leading_tabs_to_spaces(new_text, actual_profile.unit)
			} else {
				new_text.to_owned()
			};
		}
		return new_text.to_owned();
	}

	// Compute per-line indent deltas between old and actual.
	let line_count = old_profile.lines.len().min(actual_profile.lines.len());
	let mut deltas: Vec<isize> = Vec::new();
	for i in 0..line_count {
		let old_line = &old_profile.lines[i];
		let actual_line = &actual_profile.lines[i];
		if old_line.trim().is_empty() || actual_line.trim().is_empty() {
			continue;
		}
		let d = count_leading_whitespace(actual_line) as isize
			- count_leading_whitespace(old_line) as isize;
		deltas.push(d);
	}

	if deltas.is_empty() {
		return new_text.to_owned();
	}

	let delta = deltas[0];
	if !deltas.iter().all(|&v| v == delta) {
		return new_text.to_owned();
	}

	if delta == 0 {
		return new_text.to_owned();
	}

	// If new_text and actual_text use different indent chars, bail.
	if let (Some(new_ch), Some(actual_ch)) = (new_profile.char, actual_profile.char)
		&& new_ch != actual_ch
	{
		return new_text.to_owned();
	}

	let indent_char = actual_profile
		.char
		.or(old_profile.char)
		.unwrap_or_else(|| detect_indent_char(actual_text));

	let indent_str = indent_char.to_string();
	new_text
		.split('\n')
		.map(|line| {
			if line.trim().is_empty() {
				line.to_owned()
			} else if delta > 0 {
				format!("{}{}", indent_str.repeat(delta as usize), line)
			} else {
				let to_remove = ((-delta) as usize).min(count_leading_whitespace(line));
				line[to_remove..].to_owned()
			}
		})
		.collect::<Vec<_>>()
		.join("\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// ── detect_line_ending ──

	#[test]
	fn detect_lf_only() {
		assert_eq!(detect_line_ending("a\nb\nc"), LineEnding::Lf);
	}

	#[test]
	fn detect_crlf() {
		assert_eq!(detect_line_ending("a\r\nb\r\n"), LineEnding::Crlf);
	}

	#[test]
	fn detect_no_newlines() {
		assert_eq!(detect_line_ending("hello"), LineEnding::Lf);
	}

	#[test]
	fn detect_empty() {
		assert_eq!(detect_line_ending(""), LineEnding::Lf);
	}

	#[test]
	fn detect_mixed_crlf_first() {
		// CRLF appears at index 1, LF at index 2 (inside CRLF) — CRLF wins.
		assert_eq!(detect_line_ending("a\r\nb\nc"), LineEnding::Crlf);
	}

	#[test]
	fn detect_mixed_lf_first() {
		assert_eq!(detect_line_ending("a\nb\r\nc"), LineEnding::Lf);
	}

	// ── normalize_to_lf ──

	#[test]
	fn normalize_crlf_to_lf() {
		assert_eq!(normalize_to_lf("a\r\nb\r\n"), "a\nb\n");
	}

	#[test]
	fn normalize_lone_cr() {
		assert_eq!(normalize_to_lf("a\rb\r"), "a\nb\n");
	}

	#[test]
	fn normalize_lf_noop() {
		assert_eq!(normalize_to_lf("a\nb\n"), "a\nb\n");
	}

	#[test]
	fn normalize_empty() {
		assert_eq!(normalize_to_lf(""), "");
	}

	// ── strip_bom ──

	#[test]
	fn strip_bom_present() {
		let r = strip_bom("\u{FEFF}hello");
		assert_eq!(r.bom, "\u{FEFF}");
		assert_eq!(r.text, "hello");
	}

	#[test]
	fn strip_bom_absent() {
		let r = strip_bom("hello");
		assert_eq!(r.bom, "");
		assert_eq!(r.text, "hello");
	}

	#[test]
	fn strip_bom_empty() {
		let r = strip_bom("");
		assert_eq!(r.bom, "");
		assert_eq!(r.text, "");
	}

	// ── count_leading_whitespace ──

	#[test]
	fn leading_ws_spaces() {
		assert_eq!(count_leading_whitespace("    hello"), 4);
	}

	#[test]
	fn leading_ws_tabs() {
		assert_eq!(count_leading_whitespace("\t\thello"), 2);
	}

	#[test]
	fn leading_ws_mixed() {
		assert_eq!(count_leading_whitespace(" \t hello"), 3);
	}

	#[test]
	fn leading_ws_none() {
		assert_eq!(count_leading_whitespace("hello"), 0);
	}

	#[test]
	fn leading_ws_empty() {
		assert_eq!(count_leading_whitespace(""), 0);
	}

	// ── get_leading_whitespace ──

	#[test]
	fn get_leading_ws() {
		assert_eq!(get_leading_whitespace("  \thello"), "  \t");
	}

	// ── min_indent ──

	#[test]
	fn min_indent_basic() {
		assert_eq!(min_indent("  a\n    b\n  c"), 2);
	}

	#[test]
	fn min_indent_skips_blank() {
		assert_eq!(min_indent("  a\n\n    b"), 2);
	}

	#[test]
	fn min_indent_empty() {
		assert_eq!(min_indent(""), 0);
	}

	// ── detect_indent_char ──

	#[test]
	fn detect_indent_space() {
		assert_eq!(detect_indent_char("  a\n  b"), ' ');
	}

	#[test]
	fn detect_indent_tab() {
		assert_eq!(detect_indent_char("\ta\n\tb"), '\t');
	}

	#[test]
	fn detect_indent_default() {
		assert_eq!(detect_indent_char("a\nb"), ' ');
	}

	// ── normalize_for_fuzzy ──

	#[test]
	fn fuzzy_collapses_whitespace() {
		assert_eq!(normalize_for_fuzzy("  a   b  "), "a b");
	}

	#[test]
	fn fuzzy_normalizes_quotes() {
		assert_eq!(normalize_for_fuzzy("\u{201C}hi\u{201D}"), "\"hi\"");
	}

	#[test]
	fn fuzzy_normalizes_dashes() {
		assert_eq!(normalize_for_fuzzy("a\u{2014}b"), "a-b");
	}

	#[test]
	fn fuzzy_empty() {
		assert_eq!(normalize_for_fuzzy(""), "");
		assert_eq!(normalize_for_fuzzy("   "), "");
	}

	// ── normalize_unicode ──

	#[test]
	fn unicode_dashes() {
		assert_eq!(normalize_unicode("a\u{2014}b"), "a-b");
	}

	#[test]
	fn unicode_nbsp() {
		assert_eq!(normalize_unicode("a\u{00A0}b"), "a b");
	}

	#[test]
	fn unicode_zero_width() {
		assert_eq!(normalize_unicode("a\u{200B}b"), "ab");
	}

	// ── adjust_indentation ──

	#[test]
	fn adjust_no_change() {
		let text = "  foo\n  bar";
		assert_eq!(adjust_indentation(text, text, "  baz\n  qux"), "  baz\n  qux");
	}

	#[test]
	fn adjust_adds_indent() {
		let old = "foo\nbar";
		let actual = "    foo\n    bar";
		let new = "baz\nqux";
		assert_eq!(adjust_indentation(old, actual, new), "    baz\n    qux");
	}

	#[test]
	fn adjust_removes_indent() {
		let old = "    foo\n    bar";
		let actual = "  foo\n  bar";
		let new = "    baz\n    qux";
		assert_eq!(adjust_indentation(old, actual, new), "  baz\n  qux");
	}

	#[test]
	fn adjust_mixed_returns_unchanged() {
		let old = " \tfoo";
		let actual = "  foo";
		let new = " \tbaz";
		// old has mixed indent → return new unchanged.
		assert_eq!(adjust_indentation(old, actual, new), " \tbaz");
	}

	#[test]
	fn adjust_indentation_only_change() {
		// Same trimmed content, different indent → purely re-indentation → return new
		// as-is.
		let old = "foo\nbar";
		let actual = "  foo\n  bar";
		let new = "    foo\n    bar";
		assert_eq!(adjust_indentation(old, actual, new), "    foo\n    bar");
	}

	#[test]
	fn adjust_tab_to_space_conversion() {
		let old = "\tfoo\n\tbar";
		let actual = "    foo\n    bar";
		let new = "\tbaz\n\tqux";
		assert_eq!(adjust_indentation(old, actual, new), "    baz\n    qux");
	}

	#[test]
	fn adjust_empty_profiles() {
		assert_eq!(adjust_indentation("", "  foo", "baz"), "baz");
		assert_eq!(adjust_indentation("foo", "", "baz"), "baz");
		assert_eq!(adjust_indentation("foo", "  foo", ""), "");
	}

	// ── convert_leading_tabs_to_spaces ──

	#[test]
	fn tabs_to_spaces_basic() {
		assert_eq!(convert_leading_tabs_to_spaces("\t\thello", 4), "        hello");
	}

	#[test]
	fn tabs_to_spaces_mixed_skip() {
		// Mixed leading whitespace is not converted.
		assert_eq!(convert_leading_tabs_to_spaces(" \thello", 4), " \thello");
	}

	#[test]
	fn tabs_to_spaces_zero() {
		assert_eq!(convert_leading_tabs_to_spaces("\thello", 0), "\thello");
	}

	// ── restore_line_endings ──

	#[test]
	fn restore_crlf() {
		assert_eq!(restore_line_endings("a\nb\n", LineEnding::Crlf), "a\r\nb\r\n");
	}

	#[test]
	fn restore_lf_noop() {
		assert_eq!(restore_line_endings("a\nb\n", LineEnding::Lf), "a\nb\n");
	}
}
