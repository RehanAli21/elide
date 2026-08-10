use crate::constants::GRID_S;

pub fn hysteresis(probs: &[f32], enter: f32, exit: f32) -> Vec<bool> {
    let mut out = Vec::with_capacity(probs.len());
    let mut speaking = false;

    for &p in probs {
        if speaking {
            if p < exit {
                speaking = false;
            }
        } else if p >= enter {
            speaking = true;
        }

        out.push(speaking);
    }

    out
}

pub fn bridge(speech: &[bool], max_gap_s: f64) -> Vec<bool> {
    let mut out = speech.to_vec();
    let mut i = 0;

    while i < speech.len() {
        if speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && !speech[i] {
            i += 1;
        }
        let end = i;

        let has_speech_before = start > 0;
        let has_speech_after = end < speech.len();
        let len_s = (end - start) as f64 * GRID_S;

        if has_speech_before && has_speech_after && len_s < max_gap_s {
            out[start..end].fill(true);
        }
    }

    out
}

pub fn drop_bursts(speech: &[bool], min_speech_s: f64) -> (Vec<bool>, usize) {
    let mut out = speech.to_vec();
    let mut dropped = 0;
    let mut i = 0;

    while i < speech.len() {
        if !speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && speech[i] {
            i += 1;
        }
        let end = i;

        let len_s = (end - start) as f64 * GRID_S;

        if len_s < min_speech_s {
            out[start..end].fill(false);

            dropped += 1;
        }
    }

    (out, dropped)
}

pub fn pad(speech: &[bool], before_s: f64, after_s: f64) -> Vec<bool> {
    let before = (before_s / GRID_S) as usize;
    let after = (after_s / GRID_S) as usize;

    let mut out = speech.to_vec();
    let mut i = 0;

    while i < speech.len() {
        if !speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && speech[i] {
            i += 1;
        }
        let end = i;

        let from = start.saturating_sub(before);
        let to = (end + after).min(speech.len());

        out[from..to].fill(true);
    }

    out
}

pub fn longest_silences(speech: &[bool], n: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;

    while i < speech.len() {
        if speech[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && !speech[i] {
            i += 1;
        }
        runs.push((start, i));
    }

    runs.sort_by_key(|&(start, end)| std::cmp::Reverse(end - start));
    runs.truncate(n);
    runs
}
