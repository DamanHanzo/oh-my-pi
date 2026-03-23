Performs string replacements in files with fuzzy whitespace matching.

Instructions:
- Use the smallest edit that uniquely identifies the change.
- If `old_text` is not unique, expand it with more context or set `all: true`.
- Fuzzy matching handles minor whitespace and indentation differences automatically.
- Prefer editing existing files over creating new ones.

Critical rule:
- Read the file at least once before editing. The old text must come from the current file state.
