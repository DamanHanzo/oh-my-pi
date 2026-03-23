//! NAPI bindings for `libedit`.

use napi::bindgen_prelude::Result as NapiResult;
use napi_derive::napi;

use crate::{DiskFs, all_methods, hashline::format_hash_lines};

/// Metadata about an available edit method.
#[napi(object)]
pub struct MethodInfo {
	/// Method name.
	pub name:    String,
	/// LLM-facing prompt text.
	pub prompt:  String,
	/// JSON schema string.
	pub schema:  String,
	/// Formal grammar (Lark/EBNF) for the content format, if any.
	pub grammar: Option<String>,
}

/// List all built-in edit methods.
#[napi]
pub fn list_methods() -> Vec<MethodInfo> {
	all_methods()
		.into_iter()
		.map(|method| MethodInfo {
			name:    method.name().to_string(),
			prompt:  method.prompt().to_string(),
			schema:  method.schema().to_string(),
			grammar: method.grammar().map(str::to_string),
		})
		.collect()
}

/// Apply an edit using the named method against the local filesystem.
#[napi]
pub fn apply_edit(method_name: String, input: serde_json::Value) -> NapiResult<serde_json::Value> {
	let method = all_methods()
		.into_iter()
		.find(|method| method.name() == method_name)
		.ok_or_else(|| napi::Error::from_reason(format!("Unknown method: {method_name}")))?;
	let fs = DiskFs;
	let result = method
		.apply(&input, &fs)
		.map_err(|error| napi::Error::from_reason(error.to_string()))?;
	serde_json::to_value(result).map_err(|error| napi::Error::from_reason(error.to_string()))
}

/// Format file content with hashline prefixes.
#[napi]
pub fn format_hash_lines_for_js(content: String, start_line: Option<u32>) -> String {
	format_hash_lines(&content, start_line.unwrap_or(1) as usize)
}
