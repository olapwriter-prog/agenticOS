//! Noticing repetition. If you keep making the same kind of request, the OS can offer to turn it
//! into an intent (Volume 4). This module finds the pattern; it never installs anything itself.
//! Limit: only requests with the same number of words are compared.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// An intent pattern, for example "remind me to {x1} {x2} at {x3}".
    pub pattern: String,
    /// How many of the requests fit it.
    pub count: usize,
}

struct Cluster {
    fixed: Vec<Option<String>>,
    members: usize,
}

fn normalise(s: &str) -> Vec<String> {
    s.trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

fn fixed_count(f: &[Option<String>]) -> usize {
    f.iter().filter(|w| w.is_some()).count()
}

/// Find patterns that at least `min_count` of the requests share.
pub fn suggest(requests: &[&str], min_count: usize) -> Vec<Suggestion> {
    let mut clusters: Vec<Cluster> = Vec::new();
    for r in requests {
        let words = normalise(r);
        if words.is_empty() {
            continue;
        }
        let mut placed = false;
        for c in clusters.iter_mut().filter(|c| c.fixed.len() == words.len()) {
            // what the cluster would look like with this request folded in
            let merged: Vec<Option<String>> = c
                .fixed
                .iter()
                .zip(&words)
                .map(|(f, w)| match f {
                    Some(x) if x == w => Some(x.clone()),
                    _ => None,
                })
                .collect();
            // keep at least half the words fixed, and at least two, so a pattern means something
            if fixed_count(&merged) >= 2.max(words.len().div_ceil(2)) {
                c.fixed = merged;
                c.members += 1;
                placed = true;
                break;
            }
        }
        if !placed {
            clusters.push(Cluster {
                fixed: words.into_iter().map(Some).collect(),
                members: 1,
            });
        }
    }
    clusters
        .into_iter()
        .filter(|c| c.members >= min_count)
        .map(|c| {
            let mut n = 0;
            let parts: Vec<String> = c
                .fixed
                .iter()
                .map(|w| match w {
                    Some(x) => x.clone(),
                    None => {
                        n += 1;
                        format!("{{x{n}}}")
                    }
                })
                .collect();
            Suggestion {
                pattern: parts.join(" "),
                count: c.members,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_intents::Intent;

    #[test]
    fn a_repeated_shape_becomes_a_pattern() {
        let reqs = [
            "Remind me to call mum at 6pm",
            "remind me to buy milk at 7pm",
            "Remind me to pay rent at 9am",
        ];
        let s = suggest(&reqs, 3);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].pattern, "remind me to {x1} {x2} at {x3}");
        assert_eq!(s[0].count, 3);
    }
    #[test]
    fn below_the_threshold_nothing_is_suggested() {
        let reqs = [
            "remind me to call mum at 6pm",
            "remind me to buy milk at 7pm",
        ];
        assert!(suggest(&reqs, 3).is_empty());
    }
    #[test]
    fn unrelated_requests_do_not_merge() {
        let reqs = [
            "what is on monday",
            "play some jazz music please",
            "turn off the lights now",
            "remind me to call mum",
        ];
        assert!(suggest(&reqs, 2).is_empty());
    }
    #[test]
    fn the_same_request_repeated_gives_a_literal_pattern() {
        let reqs = ["what's on today?", "What's on today", "what's on today!"];
        let s = suggest(&reqs, 3);
        assert_eq!(s[0].pattern, "what's on today");
        assert_eq!(s[0].count, 3);
    }
    #[test]
    fn requests_of_different_length_are_not_compared() {
        let reqs = [
            "remind me to call mum at 6pm",
            "remind me to pay the electricity bill at 6pm",
            "remind me to call dad at 6pm",
        ];
        let s = suggest(&reqs, 2);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].count, 2);
    }
    #[test]
    fn two_separate_habits_give_two_suggestions() {
        let reqs = [
            "what is on monday",
            "remind me to call mum",
            "what is on tuesday",
            "remind me to call dad",
            "what is on friday",
            "remind me to call gran",
        ];
        let s = suggest(&reqs, 3);
        assert_eq!(s.len(), 2);
        let pats: Vec<&str> = s.iter().map(|x| x.pattern.as_str()).collect();
        assert!(pats.contains(&"what is on {x1}") && pats.contains(&"remind me to call {x1}"));
    }
    #[test]
    fn empty_requests_are_ignored() {
        assert!(suggest(&["", "   ", "..."], 1).is_empty());
    }
    #[test]
    fn a_suggested_pattern_really_matches_the_requests_it_came_from() {
        let reqs = [
            "what is on monday",
            "What is on Tuesday?",
            "what is on friday",
        ];
        let s = suggest(&reqs, 3);
        let intent = Intent::new(&s[0].pattern, "read_calendar");
        for r in reqs {
            assert!(intent.try_match(r).is_some(), "{r}");
        }
    }
}
