//! Native libedit bridge for structured file editing.

use std::collections::HashSet;

use libedit::{
	ChangeOp, CodexPatchMethod, EditFs, EditMethod, HashlineMethod, InMemoryFs, PatchMethod,
	ReplaceMethod,
	hashline::{compute_line_hash, format_hash_lines},
};
use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::task;

#[napi(object)]
pub struct LibEditMethodInfo {
	pub name:    String,
	pub prompt:  String,
	pub schema:  String,
	pub grammar: Option<String>,
}

#[napi(object)]
pub struct LibEditFileSeed {
	pub path:    String,
	pub content: String,
}

#[napi(object)]
pub struct LibEditOptions {
	#[napi(js_name = "allowFuzzy")]
	pub allow_fuzzy: Option<bool>,
	pub threshold:   Option<f64>,
}

#[napi(object)]
pub struct LibEditOperation {
	pub kind:    String,
	pub path:    String,
	pub content: Option<String>,
}

#[napi(object)]
pub struct LibEditApplyOutput {
	#[napi(js_name = "resultJson")]
	pub result_json: String,
	pub operations:  Vec<LibEditOperation>,
}

#[napi(js_name = "libEditListMethods")]
pub fn lib_edit_list_methods() -> Vec<LibEditMethodInfo> {
	libedit::all_methods()
		.iter()
		.map(|method| LibEditMethodInfo {
			name:    method.name().to_string(),
			prompt:  method.prompt().to_string(),
			schema:  method.schema().to_string(),
			grammar: method.grammar().map(str::to_string),
		})
		.collect()
}

#[napi(js_name = "libEditApply")]
pub fn lib_edit_apply(
	method_name: String,
	input_json: String,
	files: Vec<LibEditFileSeed>,
	options: Option<LibEditOptions>,
) -> task::Async<LibEditApplyOutput> {
	task::blocking("libedit_apply", (), move |_| {
		let input = serde_json::from_str(&input_json)
			.map_err(|err| Error::from_reason(format!("Invalid libedit input JSON: {err}")))?;
		let options = options.unwrap_or(LibEditOptions { allow_fuzzy: None, threshold: None });
		let allow_fuzzy = options.allow_fuzzy.unwrap_or(true);
		let threshold = options.threshold.unwrap_or(0.95);
		let method = build_method(method_name.as_str(), allow_fuzzy, threshold)?;
		let fs = InMemoryFs::with_files(files.into_iter().map(|file| (file.path, file.content)));
		let result = method
			.apply(&input, &fs)
			.map_err(|err| Error::from_reason(err.to_string()))?;
		let operations = build_operations(&result, &fs)?;
		let result_json = serde_json::to_string(&result)
			.map_err(|err| Error::from_reason(format!("Failed to serialize libedit result: {err}")))?;
		Ok(LibEditApplyOutput { result_json, operations })
	})
}

#[napi(js_name = "libEditFormatHashLines")]
pub fn lib_edit_format_hash_lines(content: String, start_line: Option<u32>) -> String {
	format_hash_lines(&content, start_line.unwrap_or(1) as usize)
}

#[napi(js_name = "libEditComputeLineHash")]
pub fn lib_edit_compute_line_hash(line_number: u32, line: String) -> String {
	compute_line_hash(line_number as usize, &line)
}

fn build_method(
	method_name: &str,
	allow_fuzzy: bool,
	threshold: f64,
) -> Result<Box<dyn EditMethod>> {
	let method: Box<dyn EditMethod> = match method_name {
		"hashline" => Box::new(HashlineMethod),
		"replace" => Box::new(ReplaceMethod::new(allow_fuzzy, threshold)),
		"patch" => Box::new(PatchMethod::new(allow_fuzzy, threshold)),
		"apply_patch" => Box::new(CodexPatchMethod::new(allow_fuzzy, threshold)),
		_ => return Err(Error::from_reason(format!("Unknown libedit method: {method_name}"))),
	};
	Ok(method)
}

fn build_operations(
	result: &libedit::EditResult,
	fs: &InMemoryFs,
) -> Result<Vec<LibEditOperation>> {
	let mut operations = Vec::new();
	let mut seen = HashSet::<(String, String)>::new();
	for change in std::iter::once(&result.change).chain(result.changes.iter()) {
		append_change_operations(change, fs, &mut operations, &mut seen)?;
	}
	Ok(operations)
}

fn append_change_operations(
	change: &libedit::FileChange,
	fs: &InMemoryFs,
	operations: &mut Vec<LibEditOperation>,
	seen: &mut HashSet<(String, String)>,
) -> Result<()> {
	match change.op {
		ChangeOp::Create => {
			let content = fs
				.read(&change.path)
				.map_err(|err| Error::from_reason(err.to_string()))?;
			push_operation(operations, seen, LibEditOperation {
				kind:    "write".to_string(),
				path:    change.path.clone(),
				content: Some(content),
			});
		},
		ChangeOp::Delete => {
			push_operation(operations, seen, LibEditOperation {
				kind:    "delete".to_string(),
				path:    change.path.clone(),
				content: None,
			});
		},
		ChangeOp::Update => {
			let write_path = change.new_path.as_deref().unwrap_or(&change.path);
			let content = fs
				.read(write_path)
				.map_err(|err| Error::from_reason(err.to_string()))?;
			push_operation(operations, seen, LibEditOperation {
				kind:    "write".to_string(),
				path:    write_path.to_string(),
				content: Some(content),
			});
			if change.new_path.is_some() {
				push_operation(operations, seen, LibEditOperation {
					kind:    "delete".to_string(),
					path:    change.path.clone(),
					content: None,
				});
			}
		},
	}
	Ok(())
}

fn push_operation(
	operations: &mut Vec<LibEditOperation>,
	seen: &mut HashSet<(String, String)>,
	operation: LibEditOperation,
) {
	let key = (
		operation.kind.clone(),
		format!("{}\u{0}{}", operation.path, operation.content.as_deref().unwrap_or_default()),
	);
	if seen.insert(key) {
		operations.push(operation);
	}
}
