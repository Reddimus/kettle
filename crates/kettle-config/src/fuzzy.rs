//! A small, dependency-free fuzzy matcher (subsequence scoring) shared by
//! the SSH launcher, the theme picker and the command palette.
//!
//! `pattern` matches `candidate` if its characters appear in order (a
//! subsequence), case-insensitively. The score rewards matches that are
//! contiguous, at word boundaries (`-`, `_`, `.`, ` `, `/`, `:`, or camelCase),
//! and especially a leading-prefix match — so `gp` ranks `gpu-box` above
//! `staging-prod`. Each candidate is scored by its best alignment, not the
//! first one found, so `theme` in `Next theme` matches the word `theme`
//! rather than the `t` of `Next`. Higher is better; `None` means no match.
//! Pure.

/// Longest pattern, in chars, aligned exactly. Longer ones, and candidates
/// longer than [`MAX_ALIGNED_CANDIDATE`], fall back to the first match of
/// each char so the work stays bounded for configured host names.
const MAX_ALIGNED_PATTERN: usize = 64;
/// Longest candidate, in chars, aligned exactly.
const MAX_ALIGNED_CANDIDATE: usize = 256;

/// One way `pattern` lies in a candidate: its score and the Unicode scalar
/// positions of the matched chars, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alignment {
    pub score: i32,
    pub positions: Vec<usize>,
}

/// Score `candidate` against `pattern`. `Some(score)` if every pattern char
/// is found in order; higher = better. An empty pattern matches everything
/// with a neutral score so callers can show the full list.
pub fn score(pattern: &str, candidate: &str) -> Option<i32> {
    align(pattern, candidate).map(|alignment| alignment.score)
}

/// Unicode scalar positions of the alignment [`score`] rates.
pub fn matched_char_positions(pattern: &str, candidate: &str) -> Option<Vec<usize>> {
    align(pattern, candidate).map(|alignment| alignment.positions)
}

/// The best-scoring alignment of `pattern` in `candidate`.
pub fn align(pattern: &str, candidate: &str) -> Option<Alignment> {
    Matcher::new(pattern, candidate)?.best(false)
}

/// The best alignment in which every matched char starts a word (the first
/// char, one after a separator, or a camelCase hump), continues the previous
/// match, or matches whitespace typed in the pattern; or else the pattern as
/// one contiguous run anywhere. `None` when the pattern only lies scattered
/// through the middles of words, as `font` does in `Focus next pane`.
pub fn align_strong(pattern: &str, candidate: &str) -> Option<Alignment> {
    let matcher = Matcher::new(pattern, candidate)?;
    let words = matcher.best(true);
    let run = matcher.best_run();
    match (words, run) {
        (Some(words), Some(run)) if run.score > words.score => Some(run),
        (words, run) => words.or(run),
    }
}

struct Matcher {
    pat: Vec<char>,
    cand: Vec<char>,
    /// `cand` folded the same way as `pat`.
    lower: Vec<char>,
}

impl Matcher {
    fn new(pattern: &str, candidate: &str) -> Option<Self> {
        // Fold each pattern char to one char, exactly as the candidate side
        // does (`to_lowercase().next()`). A full `flat_map(to_lowercase)` fold
        // can expand one char into several (e.g. `İ`→`i̇`; `ß` stays one), while
        // the candidate keeps one per position, so such a char would never match.
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let pat: Vec<char> = pattern.chars().map(fold).collect();
        let cand: Vec<char> = candidate.chars().collect();
        let lower: Vec<char> = cand.iter().copied().map(fold).collect();
        // Cheap subsequence reject before any alignment work.
        let mut rest = lower.iter();
        if !pat.iter().all(|p| rest.any(|c| c == p)) {
            return None;
        }
        Some(Self { pat, cand, lower })
    }

    /// Whether the char at `ci` starts a word.
    fn word_start(&self, ci: usize) -> bool {
        if ci == 0 {
            return true;
        }
        let prev = self.cand[ci - 1];
        matches!(prev, '-' | '_' | '.' | ' ' | '/' | ':')
            || (prev.is_lowercase() && self.cand[ci].is_uppercase())
    }

    /// What matching the char at `ci` adds, given whether the previous
    /// pattern char matched at `ci - 1`.
    fn bonus(&self, ci: usize, contiguous: bool) -> i32 {
        let mut bonus = 1;
        // Strong bonus for matching the very first character.
        if ci == 0 {
            bonus += 8;
        } else if self.word_start(ci) {
            bonus += 5;
        }
        // Continuing a run is worth as much as starting a word, and a tie
        // keeps the run, so `next` marks all of `Next tab` rather than `Nex`
        // and the `t` of `tab`.
        if contiguous && ci > 0 {
            bonus += 5;
        }
        bonus
    }

    /// Prefer shorter candidates (fewer unmatched chars).
    fn finish(&self, total: i32, positions: Vec<usize>) -> Alignment {
        let unmatched = (self.cand.len() as i32 - self.pat.len() as i32).max(0);
        Alignment {
            score: total - unmatched / 4,
            positions,
        }
    }

    /// May pattern char `pi` match at `ci` without continuing a run, in a
    /// strong alignment?
    fn may_start_run(&self, pi: usize, ci: usize) -> bool {
        self.pat[pi].is_whitespace() || self.word_start(ci)
    }

    fn best(&self, strong: bool) -> Option<Alignment> {
        if self.pat.is_empty() {
            return Some(Alignment {
                score: 0,
                positions: Vec::new(),
            });
        }
        if self.pat.len() > MAX_ALIGNED_PATTERN || self.cand.len() > MAX_ALIGNED_CANDIDATE {
            let greedy = self.greedy();
            return (!strong || self.is_strong(&greedy.positions)).then_some(greedy);
        }
        let (m, n) = (self.pat.len(), self.cand.len());
        // `best[pi][ci]`: the highest total with pattern char `pi` at `ci`,
        // and where pattern char `pi - 1` sits in that alignment.
        let mut best: Vec<Vec<Option<(i32, usize)>>> = vec![vec![None; n]; m];
        for pi in 0..m {
            // The best total, and its position, for `pi - 1` at or before
            // `ci - 2`: the alignments `ci` may follow without continuing them.
            let mut gap: Option<(i32, usize)> = None;
            for ci in 0..n {
                if pi > 0 && ci >= 2 {
                    let candidate = best[pi - 1][ci - 2].map(|(total, _)| (total, ci - 2));
                    // Strict `>` keeps the earliest position on a tie.
                    if candidate.is_some_and(|(t, _)| gap.is_none_or(|(g, _)| t > g)) {
                        gap = candidate;
                    }
                }
                if self.lower[ci] != self.pat[pi] {
                    continue;
                }
                let starts = !strong || self.may_start_run(pi, ci);
                best[pi][ci] = if pi == 0 {
                    starts.then(|| (self.bonus(ci, false), usize::MAX))
                } else {
                    let run = (ci >= 1)
                        .then(|| best[pi - 1][ci - 1])
                        .flatten()
                        .map(|(total, _)| (total + self.bonus(ci, true), ci - 1));
                    let jump = gap
                        .filter(|_| starts)
                        .map(|(total, at)| (total + self.bonus(ci, false), at));
                    match (run, jump) {
                        (Some(run), Some(jump)) if jump.0 > run.0 => Some(jump),
                        (run, jump) => run.or(jump),
                    }
                };
            }
        }
        let (mut ci, total) = best[m - 1]
            .iter()
            .enumerate()
            .filter_map(|(ci, cell)| cell.map(|(total, _)| (ci, total)))
            .fold(None, |acc: Option<(usize, i32)>, (ci, total)| match acc {
                Some((_, t)) if t >= total => acc,
                _ => Some((ci, total)),
            })?;
        let mut positions = vec![0; m];
        for pi in (0..m).rev() {
            positions[pi] = ci;
            ci = best[pi][ci].map_or(usize::MAX, |(_, prev)| prev);
        }
        Some(self.finish(total, positions))
    }

    /// The best placement of the whole pattern as one contiguous run.
    fn best_run(&self) -> Option<Alignment> {
        let m = self.pat.len();
        if m == 0 || m > self.cand.len() {
            return None;
        }
        (0..=self.cand.len() - m)
            .filter(|&start| self.lower[start..start + m] == self.pat[..])
            .map(|start| {
                let total = (start..start + m)
                    .map(|ci| self.bonus(ci, ci > start))
                    .sum();
                (total, start)
            })
            // Strict `>` keeps the earliest run on a tie.
            .fold(
                None,
                |acc: Option<(i32, usize)>, (total, start)| match acc {
                    Some((t, _)) if t >= total => acc,
                    _ => Some((total, start)),
                },
            )
            .map(|(total, start)| self.finish(total, (start..start + m).collect()))
    }

    /// The first occurrence of each pattern char, in order.
    fn greedy(&self) -> Alignment {
        let mut positions = Vec::with_capacity(self.pat.len());
        let mut total = 0;
        for (ci, &lc) in self.lower.iter().enumerate() {
            if positions.len() == self.pat.len() {
                break;
            }
            if lc == self.pat[positions.len()] {
                total += self.bonus(ci, positions.last() == Some(&ci.wrapping_sub(1)));
                positions.push(ci);
            }
        }
        self.finish(total, positions)
    }

    fn is_strong(&self, positions: &[usize]) -> bool {
        let words = positions
            .iter()
            .enumerate()
            .all(|(pi, &ci)| (pi > 0 && positions[pi - 1] + 1 == ci) || self.may_start_run(pi, ci));
        words || positions.windows(2).all(|pair| pair[0] + 1 == pair[1])
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

    /// A char whose `to_lowercase()` expands to multiple
    /// codepoints (`İ` → `i` + combining dot) must still fuzzy-match itself.
    /// Asymmetric folding (multi-char pattern vs single-char candidate) would
    /// make it never match.
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

    /// The best alignment wins over the first one found: `theme` in `Next
    /// theme` is the word, not the `t` of `Next`, and its score shows it.
    #[test]
    fn score_rates_the_best_alignment_not_the_first() {
        assert_eq!(
            matched_char_positions("theme", "Next theme"),
            Some(vec![5, 6, 7, 8, 9])
        );
        assert!(score("theme", "Next theme") > score("theme", "Toggle light/dark theme"));
        // A run is not broken to reach a later word start.
        assert_eq!(
            matched_char_positions("next", "Next tab"),
            Some(vec![0, 1, 2, 3])
        );
        assert_eq!(
            matched_char_positions("split", "Split left (new pane on the left)"),
            Some(vec![0, 1, 2, 3, 4])
        );
        // Score and positions always come from the same alignment.
        for (pattern, candidate) in [
            ("spt", "Split tab"),
            ("ct", "Cerrar pestaña"),
            ("gp", "staging-prod"),
            ("nt", "New tab"),
        ] {
            let alignment = align(pattern, candidate).unwrap();
            assert_eq!(score(pattern, candidate), Some(alignment.score));
            assert_eq!(
                matched_char_positions(pattern, candidate),
                Some(alignment.positions)
            );
        }
    }

    /// A strong alignment starts each scattered piece at a word, or is one
    /// contiguous run; whitespace typed in the pattern counts as a word start.
    #[test]
    fn strong_alignments_start_words_or_run_contiguously() {
        assert_eq!(align_strong("font", "Focus next pane"), None);
        assert!(align("font", "Focus next pane").is_some());
        assert_eq!(
            align_strong("font", "Reset font size").map(|a| a.positions),
            Some(vec![6, 7, 8, 9])
        );
        // A contiguous run in the middle of a word is strong.
        assert_eq!(
            align_strong("screen", "Toggle fullscreen").map(|a| a.positions),
            Some((11..17).collect())
        );
        // The space in the query need not sit at a word start.
        assert_eq!(
            align_strong("sp ri", "Split right").map(|a| a.positions),
            Some(vec![0, 1, 5, 6, 7])
        );
        assert_eq!(align_strong("split", "Broadcast input to all panes"), None);
        // A strong alignment may score below the best one; it is still found.
        let best = align("ab", "a-xab").unwrap();
        let strong = align_strong("ab", "a-xab").unwrap();
        assert_eq!(best.positions, vec![0, 4]);
        assert_eq!(strong.positions, vec![3, 4]);
        assert!(strong.score < best.score);
        let strong = align_strong("ab", "ax-b").unwrap();
        assert_eq!(strong.positions, vec![0, 3]);
        assert_eq!(align_strong("ab", "axyb"), None);
        assert_eq!(align_strong("", "anything").map(|a| a.score), Some(0));
    }

    /// Past the exact-alignment bounds the first match of each char is used,
    /// so a long configured host name costs no more than a linear scan.
    #[test]
    fn long_inputs_fall_back_to_the_first_match() {
        let long = format!("{}Next theme", "x".repeat(MAX_ALIGNED_CANDIDATE));
        let offset = MAX_ALIGNED_CANDIDATE;
        assert_eq!(
            matched_char_positions("theme", &long),
            Some(vec![
                offset + 3,
                offset + 6,
                offset + 7,
                offset + 8,
                offset + 9
            ])
        );
        // A word-start `t` right before the run would align better.
        let run = "x".repeat(MAX_ALIGNED_PATTERN);
        let pattern = format!("t{run}");
        let candidate = format!("at t{run}");
        let mut first = vec![1];
        first.extend(4..4 + MAX_ALIGNED_PATTERN);
        assert_eq!(matched_char_positions(&pattern, &candidate), Some(first));
        let pattern = format!("t{}", &run[1..]);
        assert_eq!(
            matched_char_positions(&pattern, &candidate).map(|p| p[0]),
            Some(3)
        );
        // Within the bounds the same candidate aligns exactly.
        let short = format!("{}Next theme", "x".repeat(MAX_ALIGNED_CANDIDATE - 20));
        assert_eq!(
            matched_char_positions("theme", &short).map(|p| p[0]),
            Some(MAX_ALIGNED_CANDIDATE - 20 + 5)
        );
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
