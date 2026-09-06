//! Deterministic gates. Every model output passes one of these before it is
//! allowed to touch anything, and self-reported confidence is ignored entirely.

/// Consonant skeleton: lowercase, letters only, vowels dropped.
fn skeleton(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
        .collect()
}

fn letters(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Levenshtein similarity, 0.0 to 1.0.
fn sim(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    let dist = prev[b.len()] as f64;
    let longest = a.len().max(b.len()) as f64;
    1.0 - dist / longest
}

/// `min` of direct and consonant-skeleton similarity — NOT `max`.
///
/// Using max let `division -> schedules` through at exactly 0.40, which is the
/// precise hallucination this gate exists to stop. Measured behaviour:
///
/// ```text
/// division   -> duration     accept   (correct)
/// blockboard -> block        accept   (correct)
/// division   -> schedules    reject   (hallucination)
/// 2020       -> BrainClean   reject   (hallucination)
/// PowerPoint -> BrainClean   reject   (correct answer, phonetically distant)
/// ```
///
/// That last row is a known limitation: product-name substitutions need a
/// separate deterministic rule (the user supplies the app name), not this gate.
pub fn phonetic_sim(a: &str, b: &str) -> f64 {
    let direct = sim(&letters(a), &letters(b));
    let skel = sim(&skeleton(a), &skeleton(b));
    direct.min(skel)
}

/// A caption fix is allowed only if it changes ONE word and that word is
/// phonetically close to what was heard. Keep-as-heard is always allowed.
pub const PHONETIC_MIN: f64 = 0.40;

pub fn caption_fix_allowed(heard: &str, proposed: &str) -> bool {
    if heard == proposed {
        return true; // keep as heard is always safe
    }
    let h: Vec<&str> = heard.split_whitespace().collect();
    let p: Vec<&str> = proposed.split_whitespace().collect();
    if h.len() != p.len() {
        return false; // single-word substitution only
    }
    let diffs: Vec<usize> = (0..h.len()).filter(|&i| h[i] != p[i]).collect();
    if diffs.len() != 1 {
        return false;
    }
    phonetic_sim(h[diffs[0]], p[diffs[0]]) >= PHONETIC_MIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_not_max_stops_the_hallucination() {
        // the row that motivated using min(): max() let this through at 0.40
        assert!(phonetic_sim("division", "schedules") < PHONETIC_MIN);
        assert!(phonetic_sim("2020", "BrainClean") < PHONETIC_MIN);
    }

    #[test]
    fn real_corrections_survive() {
        assert!(phonetic_sim("division", "duration") >= PHONETIC_MIN);
        assert!(phonetic_sim("blockboard", "block") >= PHONETIC_MIN);
    }

    #[test]
    fn only_single_word_substitutions() {
        assert!(caption_fix_allowed("the division is set", "the duration is set"));
        // two words changed
        assert!(!caption_fix_allowed("the division is set", "a duration is set"));
        // length change
        assert!(!caption_fix_allowed("the division", "the division is set"));
        // keep as heard
        assert!(caption_fix_allowed("anything at all", "anything at all"));
    }
}
