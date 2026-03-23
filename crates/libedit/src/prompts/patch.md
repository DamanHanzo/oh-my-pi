Patches files given diff hunks. This is the primary method for existing-file edits.

Hunk headers:
- `@@`: bare header when context lines are unique
- `@@ $ANCHOR`: anchor copied verbatim from the file

Rules:
- Read the target file before editing.
- Copy anchors and context lines verbatim, including whitespace.
- Use enough context lines to make a match unique.
- Keep changes inside the intended structured block.
- If a patch fails, re-read the file and generate a new patch from the current content.
- Do not use patch hunks for piecemeal formatting changes.

Operations:
- `{ path, op: "update", diff }`
- `{ path, op: "create", diff }`
- `{ path, op: "delete" }`
- `{ path, op: "update", rename, diff }`
