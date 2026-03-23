use libedit::hashline::format_line_tag;

/// Build a `LINE#ID` anchor for the provided 1-indexed line.
pub fn anchor_for(text: &str, line: usize) -> String {
	let lines: Vec<&str> = text.split('\n').collect();
	format_line_tag(line, lines[line - 1])
}
