/// Closest candidate within a small edit distance, for "did you mean" hints.
pub fn closest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let input = input.to_ascii_lowercase();
    let max = (input.len() / 3).clamp(1, 3);
    candidates
        .into_iter()
        .map(|c| (levenshtein(&input, &c.to_ascii_lowercase()), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_near_misses_only() {
        let c = ["SPOOLBOX_LISTEN", "SPOOLBOX_SMTP", "SPOOLBOX_DATABASE"];
        assert_eq!(closest("SPOOLBOX_LISTN", c), Some("SPOOLBOX_LISTEN"));
        assert_eq!(closest("SPOOLBOX_DATABSE", c), Some("SPOOLBOX_DATABASE"));
        assert_eq!(closest("SOMETHING_ELSE", c), None);
    }
}
