//! Public traits and result types for the edit subsystem.

use serde_json::Value;

use crate::{error::Result, fs::EditFs};

/// The type of file mutation that occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeOp {
	/// A file was created.
	Create,
	/// A file was updated or moved.
	Update,
	/// A file was deleted.
	Delete,
}

/// A concrete file change made by an edit method.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
	/// Operation type.
	pub op:          ChangeOp,
	/// Original path.
	pub path:        String,
	/// New path for moves/renames.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub new_path:    Option<String>,
	/// File contents before the change when available.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub old_content: Option<String>,
	/// File contents after the change when available.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub new_content: Option<String>,
}

/// Structured result returned by [`EditMethod::apply`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EditResult {
	/// Human-readable summary.
	pub message:            String,
	/// File mutation details.
	pub change:             FileChange,
	/// Additional file mutations when a method edits multiple files.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub changes:            Vec<FileChange>,
	/// Diff preview of the change, when applicable.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub diff:               Option<String>,
	/// 1-indexed line number of the first changed line in the new file.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub first_changed_line: Option<usize>,
	/// Warnings surfaced during application.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub warnings:           Vec<String>,
}

/// Preview emitted from partially streamed tool input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PartialPreview {
	/// Preview text or diff built from the partial input.
	pub preview:    String,
	/// Number of edits parsed so far.
	pub edit_count: usize,
}

/// A single edit method implementation.
pub trait EditMethod: Send + Sync {
	/// Machine-readable method name such as `"hashline"` or `"patch"`.
	fn name(&self) -> &str;

	/// Full markdown prompt shown to the LLM.
	fn prompt(&self) -> &str;

	/// JSON schema describing the tool input for this method.
	fn schema(&self) -> &str;

	/// Formal grammar (e.g. Lark/EBNF) for the tool's content format, if one
	/// exists.
	///
	/// Returns `None` for methods whose input is described entirely by the JSON
	/// schema. Returns `Some(grammar_str)` for methods that accept a structured
	/// text format (e.g. the Codex `apply_patch` format).
	fn grammar(&self) -> Option<&str> {
		None
	}

	/// Apply an edit operation using JSON input and the provided filesystem.
	fn apply(&self, input: &Value, fs: &dyn EditFs) -> Result<EditResult>;

	/// Optionally validate the provided input before application.
	fn validate(&self, _input: &Value) -> Result<()> {
		Ok(())
	}

	/// Optionally parse and preview partially streamed input.
	fn apply_partial(&self, _partial: &str, _fs: &dyn EditFs) -> Result<Option<PartialPreview>> {
		Ok(None)
	}
}
