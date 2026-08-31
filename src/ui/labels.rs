//! Condensing target labels for width-constrained legend rows.
//!
//! Fullscreen multi-target views (graph/bubble/worm legends) show every
//! target's label side by side on one row. Labels sharing a common prefix or
//! suffix word (`home-alpha`, `home-beta`) waste width on the redundant part
//! once several are shown together. This module strips shared leading/
//! trailing word-tokens first, and only if the row still doesn't fit,
//! shortens each remaining label to the shortest prefix that stays unique
//! among the set (the same idea git uses for abbreviated commit hashes).
//!
//! Transformation is applied only as far as needed to fit - the least
//! destructive result that fits is preferred over always maximally
//! shortening.

const SEPARATORS: [char; 3] = ['-', '_', '.'];

/// Byte spans of the word-tokens in `label` (runs of non-separator chars).
fn word_spans(label: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in label.char_indices() {
        if SEPARATORS.contains(&c) {
            if let Some(s) = start.take() {
                spans.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        spans.push((s, label.len()));
    }
    spans
}

/// Strips word-tokens shared by every label at the start and/or end, keeping
/// at least one token per label. `home-alpha`/`home-beta` -> `alpha`/`beta`.
fn strip_common_affixes(labels: &[&str]) -> Vec<String> {
    if labels.len() < 2 {
        return labels.iter().map(|s| s.to_string()).collect();
    }
    let spans: Vec<Vec<(usize, usize)>> = labels.iter().map(|l| word_spans(l)).collect();
    let min_tokens = spans.iter().map(|s| s.len()).min().unwrap_or(0);
    if min_tokens == 0 {
        return labels.iter().map(|s| s.to_string()).collect();
    }
    let token = |li: usize, ti: usize| -> &str { &labels[li][spans[li][ti].0..spans[li][ti].1] };

    let mut prefix = 0;
    while prefix < min_tokens - 1 && (1..labels.len()).all(|li| token(li, prefix) == token(0, prefix)) {
        prefix += 1;
    }
    let remaining_after_prefix = min_tokens - prefix;
    let mut suffix = 0;
    while suffix < remaining_after_prefix - 1
        && (1..labels.len()).all(|li| {
            let n = spans[li].len();
            let n0 = spans[0].len();
            token(li, n - 1 - suffix) == token(0, n0 - 1 - suffix)
        })
    {
        suffix += 1;
    }

    labels
        .iter()
        .enumerate()
        .map(|(li, &label)| {
            let n = spans[li].len();
            if prefix + suffix >= n {
                return label.to_string();
            }
            let start = spans[li][prefix].0;
            let end = spans[li][n - 1 - suffix].1;
            label[start..end].to_string()
        })
        .collect()
}

/// Shortens each label to the shortest prefix (>= `min_len` chars, capped at
/// the label's own length) that stays unique among the set. Labels that are
/// themselves a prefix of another label are left at full length, since no
/// shorter form of them can be unique.
fn shortest_unique_prefixes(labels: &[String], min_len: usize) -> Vec<String> {
    let chars: Vec<Vec<char>> = labels.iter().map(|l| l.chars().collect()).collect();
    let mut lens: Vec<usize> = chars.iter().map(|c| min_len.min(c.len())).collect();
    for i in 0..chars.len() {
        for j in (i + 1)..chars.len() {
            let lcp = chars[i].iter().zip(&chars[j]).take_while(|(a, b)| a == b).count();
            lens[i] = lens[i].max((lcp + 1).min(chars[i].len()));
            lens[j] = lens[j].max((lcp + 1).min(chars[j].len()));
        }
    }
    chars.iter().zip(lens).map(|(c, k)| c[..k].iter().collect()).collect()
}

/// Condenses `labels` so the row fits within `budget` columns, trying the
/// least destructive transformation first:
/// full labels -> common-affix-stripped -> shortest-unique-prefix.
/// `per_label_overhead` is the width each label costs beyond its own text
/// (bullet + separator).
pub fn condense_for_width(labels: &[&str], budget: usize, per_label_overhead: usize) -> Vec<String> {
    let width = |items: &[String]| -> usize {
        items.iter().map(|s| s.chars().count()).sum::<usize>() + per_label_overhead * items.len()
    };
    let full: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
    if labels.len() < 2 || width(&full) <= budget {
        return full;
    }
    let stripped = strip_common_affixes(labels);
    if width(&stripped) <= budget {
        return stripped;
    }
    shortest_unique_prefixes(&stripped, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_prefix_word() {
        let got = strip_common_affixes(&["home-alpha", "home-beta"]);
        assert_eq!(got, vec!["alpha", "beta"]);
    }

    #[test]
    fn strips_common_suffix_words() {
        let got = strip_common_affixes(&["east-alpha-prod", "west-alpha-prod"]);
        assert_eq!(got, vec!["east", "west"]);
    }

    #[test]
    fn keeps_at_least_one_token_when_labels_are_identical() {
        // Both tokens are shared, but at least one token per label must survive
        // even though the two results end up equal (they were already ambiguous).
        let got = strip_common_affixes(&["home-alpha", "home-alpha"]);
        assert_eq!(got, vec!["alpha", "alpha"]);
    }

    #[test]
    fn leaves_unrelated_labels_untouched() {
        let got = strip_common_affixes(&["server1", "workstation2"]);
        assert_eq!(got, vec!["server1", "workstation2"]);
    }

    #[test]
    fn unique_prefixes_shorten_to_minimal_disambiguating_length() {
        let labels = vec!["alpha".to_string(), "beta".to_string()];
        let got = shortest_unique_prefixes(&labels, 1);
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn unique_prefixes_keep_prefix_targets_at_full_length() {
        // "home" is itself a prefix of "homework" - it can never be shortened
        // and stay distinguishable, so it must stay whole.
        let labels = vec!["home".to_string(), "homework".to_string()];
        let got = shortest_unique_prefixes(&labels, 1);
        assert_eq!(got, vec!["home", "homew"]);
    }

    #[test]
    fn condense_for_width_prefers_full_labels_when_they_fit() {
        let got = condense_for_width(&["home-alpha", "home-beta"], 100, 3);
        assert_eq!(got, vec!["home-alpha", "home-beta"]);
    }

    #[test]
    fn condense_for_width_strips_affixes_before_going_to_unique_prefixes() {
        // Fits once the shared "home-" prefix is stripped, so it should stop
        // there rather than shortening further to single letters.
        let got = condense_for_width(&["home-alpha", "home-beta"], 16, 3);
        assert_eq!(got, vec!["alpha", "beta"]);
    }

    #[test]
    fn condense_for_width_falls_back_to_unique_prefixes_when_still_too_wide() {
        let got = condense_for_width(&["home-alpha", "home-beta"], 5, 3);
        assert_eq!(got, vec!["a", "b"]);
    }

    #[test]
    fn condense_for_width_single_label_is_never_touched() {
        let got = condense_for_width(&["home-alpha"], 1, 3);
        assert_eq!(got, vec!["home-alpha"]);
    }
}
