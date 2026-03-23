//! Filesystem abstraction trait and implementations.
//!
//! The core edit logic never talks to `std::fs` directly. Everything goes
//! through [`EditFs`] so the same code can run on disk, in tests, or behind an
//! FFI boundary.

use std::{collections::HashMap, path::Path, sync::Arc};

use parking_lot::RwLock;

use crate::error::{EditError, Result};

/// Minimal filesystem interface for edit operations.
pub trait EditFs: Send + Sync {
	/// Check whether a file exists.
	fn exists(&self, path: &str) -> Result<bool>;

	/// Read an entire UTF-8 file.
	fn read(&self, path: &str) -> Result<String>;

	/// Write a UTF-8 file, creating parent directories as needed.
	fn write(&self, path: &str, content: &str) -> Result<()>;

	/// Delete a file.
	fn delete(&self, path: &str) -> Result<()>;

	/// Create directories recursively.
	fn mkdir(&self, path: &str) -> Result<()>;
}

/// Production filesystem backed by `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiskFs;

impl EditFs for DiskFs {
	fn exists(&self, path: &str) -> Result<bool> {
		Ok(Path::new(path).exists())
	}

	fn read(&self, path: &str) -> Result<String> {
		std::fs::read_to_string(path)
			.map_err(|error| EditError::Io { path: path.to_string(), message: error.to_string() })
	}

	fn write(&self, path: &str, content: &str) -> Result<()> {
		if let Some(parent) = Path::new(path).parent()
			&& !parent.as_os_str().is_empty()
		{
			std::fs::create_dir_all(parent).map_err(|error| EditError::Io {
				path:    parent.display().to_string(),
				message: error.to_string(),
			})?;
		}
		std::fs::write(path, content)
			.map_err(|error| EditError::Io { path: path.to_string(), message: error.to_string() })
	}

	fn delete(&self, path: &str) -> Result<()> {
		std::fs::remove_file(path)
			.map_err(|error| EditError::Io { path: path.to_string(), message: error.to_string() })
	}

	fn mkdir(&self, path: &str) -> Result<()> {
		std::fs::create_dir_all(path)
			.map_err(|error| EditError::Io { path: path.to_string(), message: error.to_string() })
	}
}

/// In-memory filesystem for tests and embedding.
#[derive(Debug, Default, Clone)]
pub struct InMemoryFs {
	files: Arc<RwLock<HashMap<String, String>>>,
}

impl InMemoryFs {
	/// Create an empty in-memory filesystem.
	pub fn new() -> Self {
		Self::default()
	}

	/// Create an in-memory filesystem pre-populated with files.
	pub fn with_files(
		entries: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
	) -> Self {
		let files = entries
			.into_iter()
			.map(|(path, content)| (path.into(), content.into()))
			.collect();
		Self { files: Arc::new(RwLock::new(files)) }
	}

	/// Convenience getter for tests.
	pub fn get(&self, path: &str) -> Option<String> {
		self.files.read().get(path).cloned()
	}

	/// Return all stored paths.
	pub fn paths(&self) -> Vec<String> {
		self.files.read().keys().cloned().collect()
	}
}

impl EditFs for InMemoryFs {
	fn exists(&self, path: &str) -> Result<bool> {
		Ok(self.files.read().contains_key(path))
	}

	fn read(&self, path: &str) -> Result<String> {
		self
			.files
			.read()
			.get(path)
			.cloned()
			.ok_or_else(|| EditError::FileNotFound { path: path.to_string() })
	}

	fn write(&self, path: &str, content: &str) -> Result<()> {
		self
			.files
			.write()
			.insert(path.to_string(), content.to_string());
		Ok(())
	}

	fn delete(&self, path: &str) -> Result<()> {
		let removed = self.files.write().remove(path);
		if removed.is_some() {
			Ok(())
		} else {
			Err(EditError::FileNotFound { path: path.to_string() })
		}
	}

	fn mkdir(&self, _path: &str) -> Result<()> {
		Ok(())
	}
}

/// Backwards-compatible alias for older code that still refers to `RealFS`.
pub type RealFS = DiskFs;
/// Backwards-compatible alias for older code that still refers to `MemoryFS`.
pub type MemoryFS = InMemoryFs;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn memory_fs_write_read() {
		let fs = InMemoryFs::new();
		fs.write("a.txt", "hello").expect("write should succeed");
		assert!(fs.exists("a.txt").expect("exists should succeed"));
		assert_eq!(fs.read("a.txt").expect("read should succeed"), "hello");
	}

	#[test]
	fn memory_fs_not_found() {
		let fs = InMemoryFs::new();
		let err = fs.read("nope.txt").unwrap_err();
		assert!(matches!(err, EditError::FileNotFound { .. }));
	}

	#[test]
	fn memory_fs_delete() {
		let fs = InMemoryFs::new();
		fs.write("a.txt", "x").expect("write should succeed");
		fs.delete("a.txt").expect("delete should succeed");
		assert!(!fs.exists("a.txt").expect("exists should succeed"));
	}

	#[test]
	fn memory_fs_with_files() {
		let fs = InMemoryFs::with_files([("a.txt", "aaa"), ("b.txt", "bbb")]);
		assert_eq!(fs.get("a.txt"), Some("aaa".to_string()));
		assert_eq!(fs.get("b.txt"), Some("bbb".to_string()));
		assert_eq!(fs.get("c.txt"), None);
	}
}
