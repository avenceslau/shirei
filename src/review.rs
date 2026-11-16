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

/// Internal names that must not appear in reviews on public hosts.
pub const PUBLIC_DENYLIST: &[&str] = &[
    "acme",
    "acme-runner",
    "runner",
    "acme-loadtests",
    "corp.example",
    "preview.example.com",
    "rocket",
    "sprinkles",
    "rocket",
];

pub fn is_public_host(host: &str, private_hosts: &[String]) -> bool {
    !private_hosts
        .iter()
        .any(|private| private.eq_ignore_ascii_case(host))
}

/// The terms that occur in `text`, case-insensitive, where the characters
/// around the match are not letters or digits.
pub fn denylist_hits<'a>(text: &str, terms: &'a [String]) -> Vec<&'a str> {
    let haystack = text.to_ascii_lowercase();
    let bytes = haystack.as_bytes();
    terms
        .iter()
        .filter(|term| {
            let needle = term.to_ascii_lowercase();
            haystack.match_indices(&needle).any(|(index, _)| {
                let before = index.checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(index + needle.len()).copied();
                !before.is_some_and(|c| c.is_ascii_alphanumeric())
                    && !after.is_some_and(|c| c.is_ascii_alphanumeric())
            })
        })
        .map(String::as_str)
        .collect()
}

/// Where each denylisted term occurs in a review, as `title: term` and
/// `body line N: term`.
pub fn public_review_hits(
    title: Option<&str>,
    body: Option<&str>,
    terms: &[String],
) -> Vec<String> {
    let mut hits = Vec::new();
    for term in title
        .map(|title| denylist_hits(title, terms))
        .unwrap_or_default()
    {
        hits.push(format!("title: {term}"));
    }
    for (index, line) in body.unwrap_or_default().lines().enumerate() {
        for term in denylist_hits(line, terms) {
            hits.push(format!("body line {}: {term}", index + 1));
        }
    }
    hits
}

/// The `KEY-NNNN:` prefix of `title`, as `(key, number)`.
fn ticket_prefix(title: &str) -> Option<(&str, &str)> {
    let (ticket, _) = title.split_once(':')?;
    let (key, number) = ticket.split_once('-')?;
    let key_is_valid = key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let number_is_valid = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    (key_is_valid && number_is_valid).then_some((key, number))
}

/// The ticket key that recent titles use, such as `ABC` for
/// `ABC-1725: Add retries`, when at least half of them (and at least three) do.
pub fn ticket_key_from_titles(titles: &[String]) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for title in titles {
        let Some((key, _)) = ticket_prefix(title) else {
            continue;
        };
        match counts.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, count)) => *count += 1,
            None => counts.push((key, 1)),
        }
    }
    let (key, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    (count >= 3 && count * 2 >= titles.len()).then(|| key.to_string())
}

/// The first `KEY-NNNN` ticket in `text`, case-insensitive, ignoring
/// all-zero placeholders such as `ABC-000`.
pub fn find_ticket(text: &str, key: &str) -> Option<String> {
    let haystack = text.to_ascii_lowercase();
    let needle = format!("{}-", key.to_ascii_lowercase());
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(&needle) {
        let index = start + offset;
        start = index + needle.len();
        if index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            continue;
        }
        let digits: String = haystack[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let next = bytes.get(start + digits.len());
        if digits.is_empty() || next.is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        if digits.chars().all(|c| c == '0') {
            continue;
        }
        return Some(format!("{}-{digits}", key.to_ascii_uppercase()));
    }
    None
}

/// `title` with `ticket` as its `KEY-NNNN:` prefix. An existing real ticket
/// is kept; an all-zero placeholder is replaced when a ticket is known.
pub fn with_ticket(title: &str, key: &str, ticket: Option<&str>) -> String {
    if let Some((existing_key, number)) = ticket_prefix(title) {
        let is_placeholder = existing_key == key && number.chars().all(|c| c == '0');
        let Some(ticket) = ticket.filter(|_| is_placeholder) else {
            return title.to_string();
        };
        let rest = title
            .split_once(':')
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        return format!("{ticket}:{rest}");
    }
    match ticket {
        Some(ticket) => format!("{ticket}: {title}"),
        None => title.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_denylisted_terms_on_word_boundaries() {
        let terms: Vec<String> = PUBLIC_DENYLIST
            .iter()
            .map(|term| term.to_string())
            .collect();
        assert_eq!(
            public_review_hits(
                Some("Fix Acme hook"),
                Some("Runs in acme-runner\nrunners is fine\nsee x.corp.example"),
                &terms
            ),
            [
                "title: acme",
                "body line 1: acme",
                "body line 1: acme-runner",
                "body line 1: runner",
                "body line 3: corp.example"
            ]
        );
        assert!(public_review_hits(Some("Rocketry"), None, &terms).is_empty());
        assert!(!is_public_host(
            "GitLab.corp.example",
            &["gitlab.corp.example".into()]
        ));
        assert!(is_public_host("github.com", &["gitlab.corp.example".into()]));
    }

    #[test]
    fn detects_the_ticket_key_used_by_most_titles() {
        let titles = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(
            ticket_key_from_titles(&titles(&["ABC-1: a", "ABC-22: b", "fix: c", "ABC-333: d"]))
                .as_deref(),
            Some("ABC")
        );
        assert_eq!(
            ticket_key_from_titles(&titles(&["ABC-1: a", "ABC-2: b", "fix: c", "feat: d", "x"])),
            None
        );
        assert_eq!(
            ticket_key_from_titles(&titles(&["fix(scope): a", "Merge-1 thing"])),
            None
        );
    }

    #[test]
    fn finds_tickets_in_branches_and_commit_text() {
        assert_eq!(
            find_ticket("abc-1725-retry", "ABC").as_deref(),
            Some("ABC-1725")
        );
        assert_eq!(
            find_ticket("avenceslau/ABC-1725-retry", "ABC").as_deref(),
            Some("ABC-1725")
        );
        assert_eq!(
            find_ticket("fix\n\nRefs: ABC-000\nTicket: abc-42", "ABC").as_deref(),
            Some("ABC-42")
        );
        assert_eq!(find_ticket("sabc-12 abc-12a", "ABC"), None);
    }

    #[test]
    fn prefixes_or_replaces_placeholder_tickets() {
        assert_eq!(
            with_ticket("Add retries", "ABC", Some("ABC-1725")),
            "ABC-1725: Add retries"
        );
        assert_eq!(
            with_ticket("ABC-000: Add retries", "ABC", Some("ABC-1725")),
            "ABC-1725: Add retries"
        );
        assert_eq!(
            with_ticket("ABC-9: Add retries", "ABC", Some("ABC-1725")),
            "ABC-9: Add retries"
        );
        assert_eq!(with_ticket("Add retries", "ABC", None), "Add retries");
    }

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
