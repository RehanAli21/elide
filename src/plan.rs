use serde::Serialize;

use crate::constants::{
    DEAD_BRIDGE_S, EDGE_MARGIN_S, ENERGY_S, GATE_DB, GRID_S, MAX_SHRINK_S, MIN_DEAD_S,
};

pub fn dead_runs(speech: &[bool], frozen: &[bool]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;

    while i < speech.len() {
        if speech[i] || !frozen[i] {
            i += 1;
            continue;
        }

        let start = i;
        while i < speech.len() && !speech[i] && frozen[i] {
            i += 1;
        }
        runs.push((start, i));
    }

    runs
}

pub fn bridge_dead(runs: &[(usize, usize)], speech: &[bool], max_gap_s: f64) -> Vec<(usize, usize)> {
    if runs.is_empty() {
        return Vec::new();
    }

    // 1. merge runs separated by less than max_gap_s
    let mut merged: Vec<(usize, usize)> = Vec::new();
    let mut cur = runs[0];

    for &(start, end) in &runs[1..] {
        let gap_s = (start - cur.1) as f64 * GRID_S;
        if gap_s < max_gap_s {
            cur.1 = end;
        } else {
            merged.push(cur);
            cur = (start, end);
        }
    }
    merged.push(cur);

    // 2. re-apply "AND not speech" — a merge may have swallowed a word
    let mut out = Vec::new();

    for (start, end) in merged {
        let mut i = start;
        while i < end {
            if speech[i] {
                i += 1;
                continue;
            }
            let s = i;
            while i < end && !speech[i] {
                i += 1;
            }
            out.push((s, i));
        }
    }

    out
}

#[derive(Debug)]
pub enum Action {
    Keep,
    Collapse { to_s: f64 },
    Speed { factor: f64 },
}

pub fn decide(len_s: f64) -> Action {
    if len_s < 1.0 {
        Action::Keep
    } else if len_s < 4.0 {
        Action::Collapse { to_s: 0.50 }
    } else {
        let target = (len_s / 12.0).clamp(1.2, 6.0);
        Action::Speed {
            factor: (len_s / target).min(20.0),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Segment {
    pub src_start: f64,
    pub src_end: f64,
    pub speed: f64,
}

pub fn build_segments(
    runs: &[(usize, usize)],
    grid_len: usize,
    src_duration_s: f64,
) -> Vec<Segment> {
    let mut segs = Vec::new();
    let mut cursor = 0usize;

    for &(start, end) in runs {
        let len_s = (end - start) as f64 * GRID_S;
        let action = decide(len_s);

        if matches!(action, Action::Keep) {
            continue;
        }

        // normal material before this run
        if start > cursor {
            segs.push(Segment {
                src_start: cursor as f64 * GRID_S,
                src_end: start as f64 * GRID_S,
                speed: 1.0,
            });
        }

        match action {
            Action::Collapse { to_s } => segs.push(Segment {
                src_start: start as f64 * GRID_S,
                src_end: start as f64 * GRID_S + to_s,
                speed: 1.0,
            }),
            Action::Speed { factor } => segs.push(Segment {
                src_start: start as f64 * GRID_S,
                src_end: end as f64 * GRID_S,
                speed: factor,
            }),
            Action::Keep => unreachable!(),
        }

        cursor = end;
    }

    // tail
    if cursor < grid_len {
        segs.push(Segment {
            src_start: cursor as f64 * GRID_S,
            src_end: src_duration_s,
            speed: 1.0,
        });
    }

    segs
}

#[derive(Debug, Serialize)]
pub struct PlanSegment {
    pub src_start: f64,
    pub src_end: f64,
    pub speed: f64,
    pub out_start: f64,
    pub out_end: f64,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub src_duration_s: f64,
    pub out_duration_s: f64,
    pub grid_len: usize,
    pub crop: String,
    pub segments: Vec<PlanSegment>,
}

pub fn merge_adjacent(segs: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = vec![];

    for s in segs {
        match out.last_mut() {
            Some(prev) if prev.speed == s.speed && (prev.src_end - s.src_start).abs() < 1e-9 => {
                prev.src_end = s.src_end;
            }
            _ => out.push(s),
        }
    }

    out
}

pub fn trim_edges(runs: &[(usize, usize)], energy: &[f64], gate_db: f64) -> Vec<(usize, usize)> {
    let max_shrink = (MAX_SHRINK_S / ENERGY_S) as usize; // 300 energy frames
    let margin = (EDGE_MARGIN_S / ENERGY_S) as usize; // 15
    let min_dead = (MIN_DEAD_S / ENERGY_S) as usize; // 100

    let mut out = Vec::new();

    for &(a_slice, b_slice) in runs {
        // work in energy-frame indices
        let a = a_slice * 2;
        let b = (b_slice * 2).min(energy.len());

        let mut start = a;
        let mut end = b;

        // leading edge: last loud frame in the first max_shrink frames
        let window_end = (a + max_shrink).min(b);
        let mut last_loud = None;
        for i in a..window_end {
            if energy[i] > gate_db {
                last_loud = Some(i);
            }
        }
        if let Some(i) = last_loud {
            start = (i + margin).min(b - min_dead);
        }

        // trailing edge: first loud frame in the last max_shrink frames
        let window_start = b.saturating_sub(max_shrink).max(start);
        let mut first_loud = None;
        for i in window_start..b {
            if energy[i] > gate_db {
                first_loud = Some(i);
                break;
            }
        }
        if let Some(i) = first_loud {
            end = i.saturating_sub(margin).max(start + min_dead);
        }

        // back to grid slices — never drop
        out.push((start / 2, (end / 2).min(b_slice)));
    }

    out
}

/// Start times (source seconds, rounded) of the sped-up sections a given speech
/// mask would produce — dead runs >= 4 s after bridging, the min-dead filter and
/// the edge guard. This is the M4b calibration signal: the spec compares this
/// *list* across the threshold sweep, and the plateau is where it stops moving.
pub fn speedup_starts(speech: &[bool], frozen: &[bool], energy_sm: &[f64]) -> Vec<i64> {
    let runs = dead_runs(speech, frozen);
    let runs = bridge_dead(&runs, speech, DEAD_BRIDGE_S);
    let min_dead_slices = (MIN_DEAD_S / GRID_S) as usize;
    let runs: Vec<_> = runs
        .into_iter()
        .filter(|&(a, b)| b - a >= min_dead_slices)
        .collect();
    let runs = trim_edges(&runs, energy_sm, GATE_DB);
    runs.iter()
        .filter(|&&(a, b)| (b - a) as f64 * GRID_S >= 4.0)
        .map(|&(a, _)| (a as f64 * GRID_S).round() as i64)
        .collect()
}

/// Number of sped-up sections — the length of [`speedup_starts`].
pub fn count_speedups(speech: &[bool], frozen: &[bool], energy_sm: &[f64]) -> usize {
    speedup_starts(speech, frozen, energy_sm).len()
}

/// The time map: given an output timestamp, the source timestamp that plays
/// there. `check_sync` (verify) and captions (M9) both read backwards through
/// this.
pub fn map_to_source(plan: &Plan, out_t: f64) -> Option<f64> {
    for s in &plan.segments {
        if out_t >= s.out_start && out_t < s.out_end {
            let into = out_t - s.out_start;
            return Some(s.src_start + into * s.speed);
        }
    }
    None
}
