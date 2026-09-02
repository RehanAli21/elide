use crate::constants::{
    BRIDGE_GAP_S, GRID_S, MIN_SPEECH_S, PAD_AFTER_S, PAD_BEFORE_S, SAMPLES_PER_SLICE, VAD_CHUNK,
};

/// Full speech-mask pipeline from raw VAD probabilities at a given threshold:
/// hysteresis -> paint onto the 20 ms grid -> bridge -> drop bursts -> pad.
/// The M4b sweep reruns only this (the VAD probabilities do not depend on the
/// threshold), so calibration is cheap.
pub fn speech_mask(probs: &[f32], enter: f32, exit: f32, grid_len: usize) -> Vec<bool> {
    let chunk_speech = hysteresis(probs, enter, exit);

    let mut speech = vec![false; grid_len];
    for (i, slot) in speech.iter_mut().enumerate() {
        let mid_sample = i * SAMPLES_PER_SLICE + (SAMPLES_PER_SLICE / 2);
        let chunk = mid_sample / VAD_CHUNK;
        if chunk < chunk_speech.len() {
            *slot = chunk_speech[chunk];
        }
    }

    let bridged = bridge(&speech, BRIDGE_GAP_S);
    let (dropped, _) = drop_bursts(&bridged, MIN_SPEECH_S);
    pad(&dropped, PAD_BEFORE_S, PAD_AFTER_S)
}

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
