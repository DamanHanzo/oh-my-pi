//! Patch edit method implementation.

use serde_json::Value;

use crate::{
	ChangeOp, EditError, EditMethod, EditResult, Result,
	applicator::{ApplyPatchOptions, Operation, PatchInput, apply_patch},
	diff::generate_unified_diff_string,
	fs::EditFs,
};

const PROMPT: &str = include_str!("../prompts/patch.md");
const SCHEMA: &str = include_str!("../schemas/patch.json");

/// Patch edit method with configurable fuzzy matching.
pub struct PatchMethod {
	allow_fuzzy: bool,
	threshold:   f64,
}

impl PatchMethod {
	/// Create a new patch method.
	pub const fn new(allow_fuzzy: bool, threshold: f64) -> Self {
		Self { allow_fuzzy, threshold }
	}
}

impl EditMethod for PatchMethod {
	fn name(&self) -> &str {
		"patch"
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
		let op = match input.get("op").and_then(Value::as_str).unwrap_or("update") {
			"create" => Operation::Create,
			"delete" => Operation::Delete,
			_ => Operation::Update,
		};
		let rename = input
			.get("rename")
			.and_then(Value::as_str)
			.map(str::to_string);
		let diff = input
			.get("diff")
			.and_then(Value::as_str)
			.map(str::to_string);

		let result = apply_patch(
			&PatchInput { path: path.to_string(), op, rename: rename.clone(), diff },
			fs,
			ApplyPatchOptions {
				allow_fuzzy:     self.allow_fuzzy,
				fuzzy_threshold: self.threshold,
				dry_run:         false,
			},
		)?;

		let diff_result =
			match (&result.change.old_content, &result.change.new_content, result.change.op) {
				(Some(old), Some(new), ChangeOp::Update) => {
					Some(generate_unified_diff_string(old, new, 3))
				},
				_ => None,
			};

		let message = match result.change.op {
			ChangeOp::Create => format!("Created {path}"),
			ChangeOp::Delete => format!("Deleted {path}"),
			ChangeOp::Update => {
				if let Some(dest) = &rename {
					format!("Updated and moved {path} to {dest}")
				} else {
					format!("Updated {path}")
				}
			},
		};

		Ok(EditResult {
			message,
			change: result.change,
			changes: Vec::new(),
			diff: diff_result.as_ref().map(|diff| diff.diff.clone()),
			first_changed_line: diff_result.and_then(|diff| diff.first_changed_line),
			warnings: result.warnings,
		})
	}
}
