//! Hashline edit method implementation.

use serde_json::Value;

use crate::{
	ChangeOp, EditError, EditMethod, EditResult, FileChange, Result,
	diff::generate_diff_string,
	fs::EditFs,
	hashline::{
		HashlineApplyOptions, HashlineEdit, apply_hashline_edits, apply_hashline_edits_with_options,
		parse_tag, try_parse_tag,
	},
	normalize::{detect_line_ending, normalize_to_lf, restore_line_endings, strip_bom},
};

const PROMPT: &str = include_str!("../prompts/hashline.md");
const SCHEMA: &str = include_str!("../schemas/hashline.json");

/// Hashline edit method.
pub struct HashlineMethod;

impl EditMethod for HashlineMethod {
	fn name(&self) -> &str {
		"hashline"
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
		let move_to = input.get("move").and_then(Value::as_str);
		let delete_file = input
			.get("delete")
			.and_then(Value::as_bool)
			.unwrap_or(false);
		let edits_value = input
			.get("edits")
			.and_then(Value::as_array)
			.ok_or_else(|| EditError::InvalidInput { message: "missing 'edits' array".into() })?;
		let autocorrect_escaped_tabs = input.get("autocorrectEscapedTabs").and_then(Value::as_bool);

		if let Some(dest) = move_to
			&& dest == path
		{
			return Err(EditError::SamePathRename);
		}

		if delete_file {
			let old_content = if fs.exists(path)? {
				Some(fs.read(path)?)
			} else {
				None
			};
			if old_content.is_some() {
				fs.delete(path)?;
			}
			return Ok(EditResult {
				message:            format!("Deleted {path}"),
				change:             FileChange {
					op: ChangeOp::Delete,
					path: path.into(),
					new_path: None,
					old_content,
					new_content: None,
				},
				changes:            Vec::new(),
				diff:               None,
				first_changed_line: None,
				warnings:           Vec::new(),
			});
		}

		if move_to.is_some() && edits_value.is_empty() {
			let dest = move_to.expect("checked above");
			if !fs.exists(path)? {
				return Err(EditError::FileNotFound { path: path.to_string() });
			}
			let content = fs.read(path)?;
			fs.write(dest, &content)?;
			fs.delete(path)?;
			return Ok(EditResult {
				message:            format!("Moved {path} to {dest}"),
				change:             FileChange {
					op:          ChangeOp::Update,
					path:        path.into(),
					new_path:    Some(dest.into()),
					old_content: Some(content.clone()),
					new_content: Some(content),
				},
				changes:            Vec::new(),
				diff:               None,
				first_changed_line: None,
				warnings:           Vec::new(),
			});
		}

		let edits = parse_hashline_edits(edits_value)?;

		if !fs.exists(path)? {
			let mut lines = Vec::<String>::new();
			for edit in &edits {
				match edit {
					HashlineEdit::AppendFile { lines: content } => lines.extend(content.clone()),
					HashlineEdit::PrependFile { lines: content } => {
						let mut new_lines = content.clone();
						new_lines.extend(lines);
						lines = new_lines;
					},
					_ => {
						return Err(EditError::FileNotFound { path: path.to_string() });
					},
				}
			}
			let content = lines.join("\n");
			fs.write(path, &content)?;
			return Ok(EditResult {
				message:            format!("Created {path}"),
				change:             FileChange {
					op:          ChangeOp::Create,
					path:        path.into(),
					new_path:    None,
					old_content: None,
					new_content: Some(content),
				},
				changes:            Vec::new(),
				diff:               None,
				first_changed_line: Some(1),
				warnings:           Vec::new(),
			});
		}

		let raw = fs.read(path)?;
		let bom = strip_bom(&raw);
		let ending = detect_line_ending(bom.text);
		let normalized = normalize_to_lf(bom.text);

		let result = if let Some(value) = autocorrect_escaped_tabs {
			apply_hashline_edits_with_options(&normalized, &edits, HashlineApplyOptions {
				autocorrect_escaped_tabs: value,
			})?
		} else {
			apply_hashline_edits(&normalized, &edits)?
		};
		if normalized == result.text && move_to.is_none() {
			let detail = if result.noop_edits.is_empty() {
				" The edits produced identical content.".to_string()
			} else {
				let previews = result
					.noop_edits
					.iter()
					.take(3)
					.map(|noop| format!("{} => {}", noop.loc, noop.current))
					.collect::<Vec<_>>()
					.join("; ");
				format!(" The edits produced identical content. Current content: {previews}")
			};
			return Err(EditError::NoChanges { file: path.to_string(), detail });
		}

		let final_content = format!("{}{}", bom.bom, restore_line_endings(&result.text, ending));
		let dest = move_to.unwrap_or(path);
		fs.write(dest, &final_content)?;
		if move_to.is_some() {
			fs.delete(path)?;
		}

		let diff = generate_diff_string(&normalized, &result.text, 4);
		let message = if let Some(dest) = move_to {
			format!("Updated and moved {path} to {dest}")
		} else {
			format!("Updated {path}")
		};

		Ok(EditResult {
			message,
			change: FileChange {
				op:          ChangeOp::Update,
				path:        path.into(),
				new_path:    move_to.map(str::to_string),
				old_content: Some(normalized),
				new_content: Some(result.text),
			},
			changes: Vec::new(),
			diff: Some(diff.diff),
			first_changed_line: result.first_changed_line.or(diff.first_changed_line),
			warnings: result.warnings,
		})
	}
}

fn parse_hashline_edits(edits: &[Value]) -> Result<Vec<HashlineEdit>> {
	let mut parsed = Vec::with_capacity(edits.len());
	for edit in edits {
		let loc = edit
			.get("loc")
			.ok_or_else(|| EditError::InvalidInput { message: "edit missing 'loc'".into() })?;
		let content = parse_content_lines(edit.get("content"));

		if let Some(kind) = loc.as_str() {
			match kind {
				"append" => parsed.push(HashlineEdit::AppendFile { lines: content }),
				"prepend" => parsed.push(HashlineEdit::PrependFile { lines: content }),
				other => {
					return Err(EditError::InvalidInput {
						message: format!("unknown string loc: {other}"),
					});
				},
			}
			continue;
		}

		let Some(obj) = loc.as_object() else {
			return Err(EditError::InvalidInput { message: "invalid loc type".into() });
		};

		if let Some(anchor) = obj.get("append").and_then(Value::as_str) {
			parsed.push(HashlineEdit::AppendAt { pos: parse_tag(anchor)?, lines: content });
		} else if let Some(anchor) = obj.get("prepend").and_then(Value::as_str) {
			parsed.push(HashlineEdit::PrependAt { pos: parse_tag(anchor)?, lines: content });
		} else if let Some(anchor) = obj.get("line").and_then(Value::as_str) {
			parsed.push(HashlineEdit::ReplaceLine { pos: parse_tag(anchor)?, lines: content });
		} else if let Some(block) = obj.get("block").and_then(Value::as_object) {
			let pos = block
				.get("pos")
				.and_then(Value::as_str)
				.ok_or_else(|| EditError::InvalidInput { message: "block missing 'pos'".into() })?;
			let end = block
				.get("end")
				.and_then(Value::as_str)
				.ok_or_else(|| EditError::InvalidInput { message: "block missing 'end'".into() })?;
			parsed.push(HashlineEdit::ReplaceRange {
				pos:   parse_tag(pos)?,
				end:   parse_tag(end)?,
				lines: content,
			});
		} else {
			return Err(EditError::InvalidInput { message: "unknown loc shape".into() });
		}
	}
	Ok(parsed)
}

fn parse_content_lines(value: Option<&Value>) -> Vec<String> {
	let lines = match value {
		None | Some(Value::Null) => Vec::new(),
		Some(Value::Array(items)) => items
			.iter()
			.map(|item| item.as_str().unwrap_or_default().to_string())
			.collect(),
		Some(Value::String(text)) => {
			let trimmed = text.strip_suffix('\n').unwrap_or(text);
			trimmed
				.replace('\r', "")
				.split('\n')
				.map(str::to_string)
				.collect()
		},
		Some(_) => Vec::new(),
	};
	strip_new_line_prefixes(lines)
}

fn strip_new_line_prefixes(lines: Vec<String>) -> Vec<String> {
	let non_empty: Vec<&String> = lines.iter().filter(|line| !line.is_empty()).collect();
	if non_empty.is_empty() {
		return lines;
	}

	let all_hashline = non_empty.iter().all(|line| {
		line
			.split_once(':')
			.and_then(|(prefix, _)| try_parse_tag(prefix))
			.is_some()
	});
	if all_hashline {
		return lines
			.into_iter()
			.map(|line| {
				line
					.split_once(':')
					.map_or(line.clone(), |(_, rest)| rest.to_string())
			})
			.collect();
	}

	let plus_count = non_empty
		.iter()
		.filter(|line| line.starts_with('+') && !line.starts_with("++"))
		.count();
	if plus_count * 2 >= non_empty.len() {
		return lines
			.into_iter()
			.map(|line| {
				if line.starts_with('+') && !line.starts_with("++") {
					line[1..].to_string()
				} else {
					line
				}
			})
			.collect();
	}

	lines
}
