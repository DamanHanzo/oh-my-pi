Applies precise file edits using `LINE#ID` anchors from read output.

Read the file first. Copy anchors exactly from the latest read output. Batch all edits for one file in one call. After any successful edit, re-read before editing that file again.

Operations:
- `path`: file path
- `move`: optional rename target
- `delete`: optional whole-file delete
- `edits`: array of `{ loc, content }`

`loc` values:
- `"append"` / `"prepend"`: insert at end/start of file
- `{ append: "N#ID" }` / `{ prepend: "N#ID" }`: insert after/before anchored line
- `{ line: "N#ID" }`: replace exactly one anchored line
- `{ block: { pos: "N#ID", end: "N#ID" } }`: replace inclusive range

Critical rules:
- Make the minimum exact edit.
- Use anchors exactly as `N#ID` from the latest read output.
- `block` edits require both `pos` and `end`.
- Do not duplicate boundary lines when replacing blocks.
- Do not target shared boundary lines such as `} else {`, `} catch (...) {`, or `}),`.
- `content` must be literal file content with matching indentation.
- Do not use this method to reformat unrelated code.
