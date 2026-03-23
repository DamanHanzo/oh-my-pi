pub mod codex_patch_method;
pub mod hashline_method;
pub mod patch_method;
pub mod replace_method;

use crate::EditMethod;

/// Return all built-in edit methods.
static ALL_METHODS: &[&'static dyn EditMethod] = &[
	&hashline_method::HashlineMethod,
	&replace_method::ReplaceMethod::new(true, 0.95),
	&patch_method::PatchMethod::new(true, 0.95),
	&codex_patch_method::CodexPatchMethod::new(true, 0.95),
];

pub fn all_methods() -> &'static [&'static dyn EditMethod] {
	ALL_METHODS
}
