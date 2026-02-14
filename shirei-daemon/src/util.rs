pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

pub fn request_changes_prompt_notes(
    feedback: Option<&str>,
    require_pr_push: bool,
) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    if let Some(raw) = feedback {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            sections.push(format!("Reviewer feedback:\n{trimmed}"));
        }
    }

    if require_pr_push {
        sections.push(
            "Required for this rerun:\n- Commit your fix to the current PR branch.\n- Push that branch to the remote.\n- Include the pushed commit SHA in your final summary."
                .to_string(),
        );
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::request_changes_prompt_notes;

    #[test]
    fn includes_feedback_and_push_requirement() {
        let notes = request_changes_prompt_notes(Some("fix flaky test"), true).expect("notes");
        assert!(notes.contains("Reviewer feedback:"));
        assert!(notes.contains("fix flaky test"));
        assert!(notes.contains("Commit your fix"));
        assert!(notes.contains("pushed commit SHA"));
    }

    #[test]
    fn omits_empty_feedback() {
        let notes = request_changes_prompt_notes(Some("   "), true).expect("notes");
        assert!(!notes.contains("Reviewer feedback:"));
        assert!(notes.contains("Required for this rerun:"));
    }

    #[test]
    fn none_when_no_feedback_and_no_requirement() {
        assert!(request_changes_prompt_notes(None, false).is_none());
    }
}
