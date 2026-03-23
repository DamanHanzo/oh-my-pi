//! Error types for the libedit crate.
//!
//! The edit subsystem is intentionally explicit about failures so an
//! orchestrator can retry with better input instead of treating every problem
//! as an opaque "edit failed".

/// Convenience result alias used throughout the crate.
pub type Result<T, E = EditError> = std::result::Result<T, E>;

/// A single stale hashline reference detected during validation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HashMismatch {
	/// 1-indexed line number whose hash no longer matches.
	pub line:     usize,
	/// Hash supplied by the caller.
	pub expected: String,
	/// Hash computed from the current file content.
	pub actual:   String,
}

/// Unified error type for all edit operations.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
	/// The target file does not exist.
	#[error("File not found: {path}")]
	FileNotFound { path: String },

	/// The caller attempted to create a file that already exists.
	#[error("File already exists: {path}")]
	FileAlreadyExists { path: String },

	/// An anchor could not be parsed or resolved.
	#[error("Anchor not found: {anchor} in {file}")]
	AnchorNotFound { anchor: String, file: String },

	/// One or more hashline references are stale.
	#[error("{}", format_hash_mismatch_message(mismatches, context))]
	HashMismatch {
		/// Individual mismatches that were found.
		mismatches: Vec<HashMismatch>,
		/// Pre-rendered context lines with updated `LINE#ID` markers.
		context:    String,
	},

	/// The requested old text or hunk matched multiple locations.
	#[error("{}", format_ambiguous_match(file, *count, previews))]
	AmbiguousMatch {
		/// File being edited.
		file:     String,
		/// Number of ambiguous matches.
		count:    usize,
		/// Preview text to help disambiguate.
		previews: String,
	},

	/// No acceptable match could be found.
	#[error("No match found in {file}: {detail}")]
	NoMatch { file: String, detail: String },

	/// The edit would not change the file.
	#[error("No changes made to {file}.{detail}")]
	NoChanges { file: String, detail: String },

	/// Failed to parse tool input or diff hunks.
	#[error("{}", format_parse_error(message, line_number))]
	ParseError {
		/// Parse failure description.
		message:     String,
		/// Optional 1-based line number in the input that triggered the error.
		line_number: Option<usize>,
	},

	/// A single-file patch contained multiple file markers.
	#[error(
		"Diff contains {count} file markers. Single-file patches cannot contain multi-file markers."
	)]
	MultiFilePatch { count: usize },

	/// Patch hunks target overlapping ranges.
	#[error("Overlapping hunks detected in {file} at lines {range1} and {range2}.")]
	OverlappingHunks {
		/// File being edited.
		file:   String,
		/// First overlapping range.
		range1: String,
		/// Second overlapping range.
		range2: String,
	},

	/// Caller supplied malformed or incomplete JSON input.
	#[error("Invalid input: {message}")]
	InvalidInput { message: String },

	/// Caller supplied an invalid range or line hint.
	#[error("Invalid range: {message}")]
	InvalidRange { message: String },

	/// Move/rename target is identical to the source.
	#[error("Rename path is the same as source path")]
	SamePathRename,

	/// Generic match failure retained for helper-level APIs.
	#[error("{path}: {message}")]
	MatchError { path: String, message: String },

	/// Generic application failure retained for helper-level APIs.
	#[error("{message}")]
	ApplyError { message: String },

	/// Generic validation failure retained for helper-level APIs.
	#[error("Validation error: {message}")]
	ValidationError { message: String },

	/// Filesystem I/O error.
	#[error("I/O error on {path}: {message}")]
	Io { path: String, message: String },

	/// JSON serialization/deserialization error.
	#[error(transparent)]
	JsonError(#[from] serde_json::Error),
}

impl From<std::io::Error> for EditError {
	fn from(error: std::io::Error) -> Self {
		Self::Io { path: "<io>".to_string(), message: error.to_string() }
	}
}

fn format_parse_error(message: &str, line_number: &Option<usize>) -> String {
	match line_number {
		Some(n) => format!("Line {n}: {message}"),
		None => message.to_string(),
	}
}

fn format_hash_mismatch_message(mismatches: &[HashMismatch], context: &str) -> String {
	let count = mismatches.len();
	let verb = if count == 1 { "has" } else { "have" };
	format!(
		"{count} line{} {verb} changed since last read. Use the updated LINE#ID references shown \
		 below (>>> marks changed lines).\n\n{context}",
		if count == 1 { "" } else { "s" }
	)
}

fn format_ambiguous_match(file: &str, count: usize, previews: &str) -> String {
	if previews.trim().is_empty() {
		return format!("Found {count} occurrences in {file}. Add more context to disambiguate.");
	}
	format!(
		"Found {count} occurrences in {file}.\n\n{previews}\n\nAdd more context to disambiguate."
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_error_with_line_number() {
		let err =
			EditError::ParseError { message: "unexpected token".into(), line_number: Some(42) };
		assert_eq!(err.to_string(), "Line 42: unexpected token");
	}

	#[test]
	fn parse_error_without_line_number() {
		let err = EditError::ParseError { message: "empty input".into(), line_number: None };
		assert_eq!(err.to_string(), "empty input");
	}

	#[test]
	fn hash_mismatch_message_includes_context() {
		let err = EditError::HashMismatch {
			mismatches: vec![HashMismatch {
				line:     3,
				expected: "ZZ".into(),
				actual:   "PM".into(),
			}],
			context:    ">>> 3#PM:changed".into(),
		};
		let rendered = err.to_string();
		assert!(rendered.contains("changed since last read"));
		assert!(rendered.contains(">>> 3#PM:changed"));
	}

	#[test]
	fn ambiguous_match_includes_file() {
		let err = EditError::AmbiguousMatch {
			file:     "src/lib.rs".into(),
			count:    2,
			previews: "  10 | foo".into(),
		};
		assert!(err.to_string().contains("src/lib.rs"));
		assert!(err.to_string().contains("2 occurrences"));
	}
}
