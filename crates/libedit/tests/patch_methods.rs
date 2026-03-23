use libedit::{ChangeOp, EditError, EditFs, EditMethod, InMemoryFs, PatchMethod};
use serde_json::json;

#[test]
fn patch_method_updates_existing_file_with_context_hunk() {
	let fs = InMemoryFs::with_files([("src/lib.rs", "fn main() {\n    println!(\"hello\");\n}\n")]);
	let method = PatchMethod::new(true, 0.95);

	let result = method
        .apply(
            &json!({
                "path": "src/lib.rs",
                "diff": "@@ fn main() {\n fn main() {\n-    println!(\"hello\");\n+    println!(\"world\");\n }\n"
            }),
            &fs,
        )
        .expect("patch should succeed");

	let updated = fs.read("src/lib.rs").expect("file should exist");
	assert!(updated.contains("world"));
	assert_eq!(result.change.op, ChangeOp::Update);
	assert!(result.diff.as_deref().unwrap_or_default().contains("@@"));
}

#[test]
fn patch_method_supports_create_delete_and_rename() {
	let fs = InMemoryFs::with_files([("old.txt", "old\n")]);
	let method = PatchMethod::new(true, 0.95);

	let created = method
		.apply(
			&json!({
				 "path": "new.txt",
				 "op": "create",
				 "diff": "hello\n"
			}),
			&fs,
		)
		.expect("create should succeed");
	assert_eq!(created.change.op, ChangeOp::Create);
	assert!(fs.exists("new.txt").expect("exists should succeed"));

	let renamed = method
		.apply(
			&json!({
				 "path": "old.txt",
				 "rename": "moved.txt",
				 "diff": "@@\n-old\n+new\n"
			}),
			&fs,
		)
		.expect("rename update should succeed");
	assert_eq!(renamed.change.new_path.as_deref(), Some("moved.txt"));
	assert!(!fs.exists("old.txt").expect("exists should succeed"));
	assert_eq!(fs.read("moved.txt").expect("file should exist"), "new\n");

	let deleted = method
		.apply(
			&json!({
				 "path": "new.txt",
				 "op": "delete"
			}),
			&fs,
		)
		.expect("delete should succeed");
	assert_eq!(deleted.change.op, ChangeOp::Delete);
	assert!(!fs.exists("new.txt").expect("exists should succeed"));
}

#[test]
fn patch_method_rejects_multi_file_patch() {
	let fs = InMemoryFs::with_files([("a.txt", "a\n")]);
	let method = PatchMethod::new(true, 0.95);

	let err = method
		.apply(
			&json!({
				 "path": "a.txt",
				 "diff": "*** Update File: a.txt\n@@\n-a\n+b\n*** Update File: b.txt\n@@\n-x\n+y\n"
			}),
			&fs,
		)
		.expect_err("multi-file patch should fail");

	assert!(matches!(err, EditError::MultiFilePatch { .. } | EditError::ApplyError { .. }));
}
