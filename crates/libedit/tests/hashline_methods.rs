mod common;

use common::anchor_for;
use libedit::{ChangeOp, EditError, EditFs, EditMethod, HashlineMethod, InMemoryFs};
use serde_json::json;

#[test]
fn hashline_method_updates_existing_file() {
	let fs = InMemoryFs::with_files([("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n")]);
	let method = HashlineMethod;
	let original = fs.read("src/main.rs").expect("file should exist");
	let line_two = anchor_for(&original, 2);

	let result = method
		.apply(
			&json!({
				 "path": "src/main.rs",
				 "edits": [{
					  "loc": { "line": line_two },
					  "content": ["    println!(\"world\");"]
				 }]
			}),
			&fs,
		)
		.expect("hashline edit should succeed");

	let updated = fs.read("src/main.rs").expect("file should still exist");
	assert!(updated.contains("world"));
	assert_eq!(result.change.op, ChangeOp::Update);
	assert!(
		result
			.diff
			.as_deref()
			.unwrap_or_default()
			.contains("+2|    println!(\"world\");")
	);
}

#[test]
fn hashline_method_create_delete_and_move_only() {
	let fs = InMemoryFs::new();
	let method = HashlineMethod;

	let created = method
		.apply(
			&json!({
				 "path": "notes.txt",
				 "edits": [{ "loc": "append", "content": ["hello", "world"] }]
			}),
			&fs,
		)
		.expect("create should succeed");
	assert_eq!(created.change.op, ChangeOp::Create);
	assert_eq!(fs.read("notes.txt").expect("created file should exist"), "hello\nworld");

	let moved = method
		.apply(
			&json!({
				 "path": "notes.txt",
				 "move": "archive/notes.txt",
				 "edits": []
			}),
			&fs,
		)
		.expect("move-only should succeed");
	assert_eq!(moved.change.new_path.as_deref(), Some("archive/notes.txt"));
	assert!(!fs.exists("notes.txt").expect("exists should succeed"));
	assert!(
		fs.exists("archive/notes.txt")
			.expect("exists should succeed")
	);

	let deleted = method
		.apply(
			&json!({
				 "path": "archive/notes.txt",
				 "delete": true,
				 "edits": []
			}),
			&fs,
		)
		.expect("delete should succeed");
	assert_eq!(deleted.change.op, ChangeOp::Delete);
	assert!(
		!fs.exists("archive/notes.txt")
			.expect("exists should succeed")
	);
}

#[test]
fn hashline_method_reports_hash_mismatch_and_noop() {
	let fs = InMemoryFs::with_files([("file.txt", "alpha\nbeta\n")]);
	let method = HashlineMethod;
	let anchor = anchor_for("alpha\nbeta\n", 1);

	fs.write("file.txt", "changed\nbeta\n")
		.expect("write should succeed");
	let mismatch = method
		.apply(
			&json!({
				 "path": "file.txt",
				 "edits": [{ "loc": { "line": anchor }, "content": ["omega"] }]
			}),
			&fs,
		)
		.expect_err("stale anchor should fail");
	assert!(matches!(mismatch, EditError::HashMismatch { .. }));

	let current = fs.read("file.txt").expect("file should exist");
	let fresh_anchor = anchor_for(&current, 1);
	let noop = method
		.apply(
			&json!({
				 "path": "file.txt",
				 "edits": [{ "loc": { "line": fresh_anchor }, "content": ["changed"] }]
			}),
			&fs,
		)
		.expect_err("noop edit should fail");
	assert!(matches!(noop, EditError::NoChanges { .. }));
}
