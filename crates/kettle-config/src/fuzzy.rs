//! A small, dependency-free fuzzy matcher (subsequence scoring) shared by
//! the SSH launcher and any future command palette.
//!
//! `pattern` matches `candidate` if its characters appear in order (a
//! subsequence), case-insensitively. The score rewards matches that are
//! contiguous, at word boundaries (`-`, `_`, `.`, ` `, `/`, or camelCase),
//! and especially a leading-prefix match — so `gp` ranks `gpu-box` above
//! `staging-prod`. Higher is better; `None` means no match. Pure.

/// Score `candidate` against `pattern`. `Some(score)` if every pattern char
/// is found in order; higher = better. An empty pattern matches everything
/// with a neutral score so callers can show the full list.
pub fn score(pattern: &str, candidate: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    // Cycle 857 (audit): fold the pattern one char→one char, exactly as the
    // candidate side does below (`cc.to_lowercase().next()`). The old
    // `flat_map(to_lowercase)` expanded a multi-codepoint fold (e.g. `İ`→`i̇`,
    // `ß` stays) into several pattern chars while the candidate kept one per
    // position, so such characters never matched. Symmetric single-char folding
    // keeps the positional walk consistent.
    let pat: Vec<char> = pattern
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let cand: Vec<char> = candidate.chars().collect();

    let mut pi = 0usize;
    let mut total = 0i32;
    let mut prev_match: Option<usize> = None;
    for (ci, &cc) in cand.iter().enumerate() {
        if pi >= pat.len() {
            break;
        }
        let lc = cc.to_lowercase().next().unwrap_or(cc);
        if lc != pat[pi] {
            continue;
        }
        let mut bonus = 1;
        // Strong bonus for matching the very first character.
        if ci == 0 {
            bonus += 8;
        } else {
            let prev = cand[ci - 1];
            let boundary = matches!(prev, '-' | '_' | '.' | ' ' | '/' | ':');
            let camel = prev.is_lowercase() && cc.is_uppercase();
            if boundary || camel {
                bonus += 5;
            }
        }
        // Contiguous run with the previous matched char.
        if prev_match == Some(ci.saturating_sub(1)) && ci > 0 {
            bonus += 4;
        }
        total += bonus;
        prev_match = Some(ci);
        pi += 1;
    }
    if pi == pat.len() {
        // Prefer shorter candidates and earlier completion (less gap).
        Some(total - (cand.len() as i32 - pat.len() as i32).max(0) / 4)
    } else {
        None
    }
}

/// The best-scoring item from `items` (by its string projection) for
/// `pattern`, or `None` if nothing matches. Ties keep the earliest item.
pub fn best<'a, T>(pattern: &str, items: &'a [T], key: impl Fn(&T) -> &str) -> Option<&'a T> {
    let mut best: Option<(i32, &T)> = None;
    for it in items {
        if let Some(s) = score(pattern, key(it)) {
            // Strict `>` so the earliest item wins a tie.
            if best.is_none_or(|(bs, _)| s > bs) {
                best = Some((s, it));
            }
        }
    }
    best.map(|(_, it)| it)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycle 857: a char whose `to_lowercase()` expands to multiple
    /// codepoints (`İ` → `i` + combining dot) must still fuzzy-match itself.
    /// The old asymmetric folding (multi-char pattern vs single-char candidate)
    /// made it never match.
    #[test]
    fn score_matches_multi_codepoint_case_fold() {
        assert!(score("İ", "İ").is_some(), "a char must fuzzy-match itself");
        // Ordinary case-insensitive matching is unaffected.
        assert!(score("GP", "gpu-box").is_some());
        assert!(score("gp", "GPU-BOX").is_some());
    }

    #[test]
    fn subsequence_matching() {
        assert!(score("gp", "gpu-box").is_some());
        assert!(score("gpb", "gpu-box").is_some(), "g,p,b in order");
        assert!(score("bpg", "gpu-box").is_none(), "out of order");
        assert!(score("xyz", "gpu-box").is_none());
        // Case-insensitive.
        assert!(score("GPU", "gpu-box").is_some());
        // Empty pattern matches anything.
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn prefix_and_boundary_outrank_scattered() {
        // "gp" should prefer the prefix/word-boundary match.
        let prefix = score("gp", "gpu-prod").unwrap();
        let scattered = score("gp", "staging-prod").unwrap();
        assert!(
            prefix > scattered,
            "prefix {prefix} should beat scattered {scattered}"
        );
        // Word-boundary (after '-') beats mid-word.
        let boundary = score("p", "gpu-prod").unwrap();
        let midword = score("p", "appserver").unwrap();
        assert!(boundary > midword);
    }

    #[test]
    fn best_picks_top_and_handles_ties() {
        let hosts = [
            ("box".to_string(), "me@a".to_string()),
            ("gpu".to_string(), "me@b".to_string()),
            ("gpu-prod".to_string(), "me@c".to_string()),
        ];
        let pick = best("gpu", &hosts, |h| h.0.as_str()).unwrap();
        assert_eq!(pick.0, "gpu", "exact-ish shortest wins");
        assert!(best("zzz", &hosts, |h| h.0.as_str()).is_none());
        // Empty pattern → first item (neutral score, earliest on tie).
        assert_eq!(best("", &hosts, |h| h.0.as_str()).unwrap().0, "box");
    }
}
