//! # libedit
//!
//! Pure Rust library for structured file editing with four built-in methods:
//! - **Hashline**: line-addressed edits using content hashes for integrity
//! - **Replace**: find-and-replace with fuzzy whitespace matching
//! - **Patch**: unified diff parsing and application with fuzzy hunk matching
//! - **Apply Patch**: Codex-style `apply_patch` parsing and application
//!
//! The crate is designed for embedding in coding agents. The public surface is
//! intentionally small: a filesystem trait, a method trait, the built-in edit
//! methods, and reusable helper modules for normalization, fuzzy matching, and
//! patch/hashline application.

pub mod applicator;
pub mod diff;
pub mod error;
pub mod fs;
pub mod fuzzy;
pub mod hashline;
pub mod methods;
pub mod normalize;
pub mod parser;
pub mod traits;

#[cfg(feature = "napi")]
pub mod napi_bindings;

#[cfg(feature = "wasm")]
pub mod wasm_bindings;

pub use error::{EditError, HashMismatch, Result};
/// Backwards-compatible alias for older code using the previous trait name.
pub use fs::EditFs as MinFS;
pub use fs::{DiskFs, EditFs, InMemoryFs, MemoryFS, RealFS};
pub use methods::{
	all_methods, codex_patch_method::CodexPatchMethod, hashline_method::HashlineMethod,
	patch_method::PatchMethod, replace_method::ReplaceMethod,
};
pub use traits::{ChangeOp, EditMethod, EditResult, FileChange, PartialPreview};
