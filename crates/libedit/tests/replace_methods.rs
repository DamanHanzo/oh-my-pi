use libedit::{ChangeOp, EditError, EditFs, EditMethod, InMemoryFs, ReplaceMethod};
use serde_json::json;

#[test]
fn replace_method_updates_unique_match_and_preserves_crlf() {
	let fs = InMemoryFs::with_files([("app.ts", "const value = 1;\r\nconst answer = 41;\r\n")]);
	let method = ReplaceMethod::new(true, 0.95);

	let result = method
		.apply(
			&json!({
				 "path": "app.ts",
				 "old_text": "const answer = 41;\n",
				 "new_text": "const answer = 42;\n"
			}),
			&fs,
		)
		.expect("replace should succeed");

	let updated = fs.read("app.ts").expect("file should exist");
	assert!(updated.contains("42"));
	assert!(updated.contains("\r\n"));
	assert_eq!(result.change.op, ChangeOp::Update);
	assert_eq!(result.first_changed_line, Some(2));
}

#[test]
fn replace_method_rejects_ambiguous_match() {
	let fs = InMemoryFs::with_files([("dup.txt", "foo\nbar\nfoo\n")]);
	let method = ReplaceMethod::new(true, 0.95);

	let err = method
		.apply(
			&json!({
				 "path": "dup.txt",
				 "old_text": "foo",
				 "new_text": "baz"
			}),
			&fs,
		)
		.expect_err("ambiguous replace should fail");

	assert!(matches!(err, EditError::AmbiguousMatch { .. }));
	assert_eq!(fs.read("dup.txt").expect("file should exist"), "foo\nbar\nfoo\n");
}
