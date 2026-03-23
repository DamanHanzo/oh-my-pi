//! Replace edit method implementation.

use serde_json::Value;

use crate::{
	ChangeOp, EditError, EditMethod, EditResult, FileChange, Result,
	diff::{ReplaceOptions, generate_diff_string, replace_text},
	fs::EditFs,
	fuzzy::find_match,
	normalize::{detect_line_ending, normalize_to_lf, restore_line_endings, strip_bom},
};

const PROMPT: &str = include_str!("../prompts/replace.md");
const SCHEMA: &str = include_str!("../schemas/replace.json");

/// Replace edit method with configurable fuzzy matching.
pub struct ReplaceMethod {
	allow_fuzzy: bool,
	threshold:   f64,
}

impl ReplaceMethod {
	/// Create a new replace method.
	pub const fn new(allow_fuzzy: bool, threshold: f64) -> Self {
		Self { allow_fuzzy, threshold }
	}
}

impl EditMethod for ReplaceMethod {
	fn name(&self) -> &str {
		"replace"
	}

	fn prompt(&self) -> &str {
		PROMPT
	}

	fn schema(&self) -> &str {
		SCHEMA
	}

	fn apply(&self, input: &Value, fs: &dyn EditFs) -> Result<EditResult> {
		let path = input
			.get("path")
			.and_then(Value::as_str)
			.ok_or_else(|| EditError::InvalidInput { message: "missing 'path'".into() })?;
		let old_text = input
			.get("old_text")
			.and_then(Value::as_str)
			.ok_or_else(|| EditError::InvalidInput { message: "missing 'old_text'".into() })?;
		let new_text = input
			.get("new_text")
			.and_then(Value::as_str)
			.ok_or_else(|| EditError::InvalidInput { message: "missing 'new_text'".into() })?;
		let replace_all = input.get("all").and_then(Value::as_bool).unwrap_or(false);

		if old_text.is_empty() {
			return Err(EditError::InvalidInput { message: "old_text must not be empty".into() });
		}
		if !fs.exists(path)? {
			return Err(EditError::FileNotFound { path: path.to_string() });
		}

		let raw = fs.read(path)?;
		let bom = strip_bom(&raw);
		let ending = detect_line_ending(bom.text);
		let content = normalize_to_lf(bom.text);
		let normalized_old = normalize_to_lf(old_text);
		let normalized_new = normalize_to_lf(new_text);

		let preflight = find_match(&content, &normalized_old, self.allow_fuzzy, Some(self.threshold));
		if !replace_all
			&& let Some(count) = preflight.occurrences
			&& count > 1
		{
			return Err(EditError::AmbiguousMatch {
				file: path.to_string(),
				count,
				previews: preflight.occurrence_previews.join("\n\n"),
			});
		}

		let result = replace_text(&content, &normalized_old, &normalized_new, &ReplaceOptions {
			fuzzy:     self.allow_fuzzy,
			all:       replace_all,
			threshold: Some(self.threshold),
		})?;

		if result.count == 0 {
			let detail = if let Some(closest) = preflight.closest {
				format!(
					"Closest match ({}% similar) at line {}.",
					(closest.confidence * 100.0).round(),
					closest.start_line
				)
			} else if self.allow_fuzzy {
				"Could not find a close enough match.".to_string()
			} else {
				"Could not find the exact text.".to_string()
			};
			return Err(EditError::NoMatch { file: path.to_string(), detail });
		}

		if result.content == content {
			return Err(EditError::NoChanges {
				file:   path.to_string(),
				detail: " The replacement produced identical content.".into(),
			});
		}

		let final_content = format!("{}{}", bom.bom, restore_line_endings(&result.content, ending));
		fs.write(path, &final_content)?;

		let diff = generate_diff_string(&content, &result.content, 4);
		let message = if result.count > 1 {
			format!("Replaced {} occurrences in {path}", result.count)
		} else {
			format!("Replaced text in {path}")
		};

		Ok(EditResult {
			message,
			change: FileChange {
				op:          ChangeOp::Update,
				path:        path.into(),
				new_path:    None,
				old_content: Some(content),
				new_content: Some(result.content),
			},
			changes: Vec::new(),
			diff: Some(diff.diff),
			first_changed_line: diff.first_changed_line,
			warnings: Vec::new(),
		})
	}
}
