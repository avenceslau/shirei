//! Rules Shirei applies to review titles and descriptions, so the
//! deterministic parts are produced by code instead of the author.

/// The first Markdown heading named "Stack" or "Stack position", outside
/// fenced code. Shirei writes stack navigation itself.
pub fn stack_heading(body: &str) -> Option<String> {
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let Some(text) = trimmed[hashes..].strip_prefix(' ') else {
            continue;
        };
        let text = text
            .trim()
            .trim_end_matches('#')
            .trim()
            .trim_end_matches(':');
        if text.eq_ignore_ascii_case("stack") || text.eq_ignore_ascii_case("stack position") {
            return Some(trimmed.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_stack_headings_outside_code() {
        assert_eq!(
            stack_heading("Why\n\n## Stack\n- after !12").as_deref(),
            Some("## Stack")
        );
        assert_eq!(
            stack_heading("### Stack position:\n1 of 2").as_deref(),
            Some("### Stack position:")
        );
        assert_eq!(stack_heading("```\n## Stack\n```\n## Tech stack"), None);
        assert_eq!(stack_heading("#Stack\nStack: 1 of 2"), None);
    }
}
