//! WASM bindings for `libedit`.

use js_sys::Function;
use wasm_bindgen::prelude::*;

use crate::{EditError, EditFs, all_methods, hashline::format_hash_lines};

struct WasmFs {
	exists: Function,
	read:   Function,
	write:  Function,
	delete: Function,
	mkdir:  Function,
}

impl EditFs for WasmFs {
	fn exists(&self, path: &str) -> crate::Result<bool> {
		self
			.exists
			.call1(&JsValue::NULL, &JsValue::from_str(path))
			.map_err(js_error(path))?
			.as_bool()
			.ok_or_else(|| EditError::Io {
				path:    path.to_string(),
				message: "exists callback did not return a boolean".into(),
			})
	}

	fn read(&self, path: &str) -> crate::Result<String> {
		self
			.read
			.call1(&JsValue::NULL, &JsValue::from_str(path))
			.map_err(js_error(path))?
			.as_string()
			.ok_or_else(|| EditError::Io {
				path:    path.to_string(),
				message: "read callback did not return a string".into(),
			})
	}

	fn write(&self, path: &str, content: &str) -> crate::Result<()> {
		self
			.write
			.call2(&JsValue::NULL, &JsValue::from_str(path), &JsValue::from_str(content))
			.map_err(js_error(path))?;
		Ok(())
	}

	fn delete(&self, path: &str) -> crate::Result<()> {
		self
			.delete
			.call1(&JsValue::NULL, &JsValue::from_str(path))
			.map_err(js_error(path))?;
		Ok(())
	}

	fn mkdir(&self, path: &str) -> crate::Result<()> {
		self
			.mkdir
			.call1(&JsValue::NULL, &JsValue::from_str(path))
			.map_err(js_error(path))?;
		Ok(())
	}
}

fn js_error(path: &str) -> impl Fn(JsValue) -> EditError + '_ {
	move |value| EditError::Io { path: path.to_string(), message: format!("{value:?}") }
}

/// List all built-in edit methods.
#[wasm_bindgen(js_name = listMethods)]
pub fn list_methods() -> Result<JsValue, JsValue> {
	let methods = all_methods()
		.into_iter()
		.map(|method| {
			serde_json::json!({
				 "name": method.name(),
				 "prompt": method.prompt(),
				 "schema": method.schema(),
				 "grammar": method.grammar(),
			})
		})
		.collect::<Vec<_>>();
	serde_wasm_bindgen::to_value(&methods).map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Apply an edit using a JavaScript-backed filesystem.
#[wasm_bindgen(js_name = applyEdit)]
pub fn apply_edit(
	method_name: &str,
	input: JsValue,
	exists: Function,
	read: Function,
	write: Function,
	delete: Function,
	mkdir: Function,
) -> Result<JsValue, JsValue> {
	let input: serde_json::Value = serde_wasm_bindgen::from_value(input)
		.map_err(|error| JsValue::from_str(&error.to_string()))?;
	let method = all_methods()
		.into_iter()
		.find(|method| method.name() == method_name)
		.ok_or_else(|| JsValue::from_str(&format!("Unknown method: {method_name}")))?;
	let fs = WasmFs { exists, read, write, delete, mkdir };
	let result = method
		.apply(&input, &fs)
		.map_err(|error| JsValue::from_str(&error.to_string()))?;
	serde_wasm_bindgen::to_value(&result).map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Format file content with hashline prefixes.
#[wasm_bindgen(js_name = formatHashLines)]
pub fn format_hash_lines_for_wasm(content: &str, start_line: Option<u32>) -> String {
	format_hash_lines(content, start_line.unwrap_or(1) as usize)
}
