//! M11 — disfluency removal. The first thing that cuts *inside* speech, so
//! every candidate must survive a gauntlet before it is allowed.
//!
//! WHAT to cut comes from the word sequence. WHERE to cut comes from the
//! waveform. Never the other way round.
//!
//! Two kinds: fillers and repeats. (Stutters are kept as a one-word repeat,
//! which the reference does not have — they are a superset, gated identically.)
//! SEARCH_WORDS bounds how far ahead to look for the next take; the 3.0 s chain
//! break decides whether a take found there still belongs to the same stumble.
//! Different jobs, not competing proxies.

use crate::captions::Word;
use crate::constants::{
    CUT_SPACING_S, ENERGY_S, FILLER_GAP_S, FILLER_MAX_S, FILLER_TAIL_S, KEEP_GAP_S, MAX_CUT_S,
    GATE_DB, MAX_TAKE_GAP_S, MIN_CUT_S, MIN_PHRASE, QUIET_DB, QUIET_RUN_S, SEARCH_WORDS, SIM_MIN, SNAP_S,
    STUTTER_GAP_S,
};
use crate::dsp::similarity;
use crate::plan::Segment;

const FILLER: [&str; 7] = ["so", "okay", "now", "ok", "also", "then", "and"];

#[derive(Debug, Clone)]
pub struct Cut {
    pub start: f64,
    pub end: f64,
    pub kind: &'static str,
    pub text: String,
    /// spectral correlation between the two takes (1.0 where not applicable)
    pub sim: f64,
    /// how many takes of the phrase the chain collected (1 where n/a)
    pub takes: usize,
    /// which splice alignment actually won. keep-last is the primary; winning
    /// on a fallback means keep-last's endpoints failed the gates.
    pub align: &'static str,
}

/// Why a candidate was thrown out — kept so the run can report it.
#[derive(Debug)]
pub struct Rejected {
    pub start: f64,
    pub kind: &'static str,
    pub why: String,
}

/// A proposal, carrying every splice alignment worth trying. The gate takes the
/// FIRST that passes, not the best — removing one copy beats removing none.
struct Candidate {
    opts: Vec<(f64, f64, &'static str)>,
    kind: &'static str,
    text: String,
    sim: f64,
    takes: usize,
}

impl Candidate {
    fn at(&self) -> f64 {
        self.opts[0].0
    }
}

/// normalise a word for comparison: lowercase, no surrounding punctuation
fn norm(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn frame(t: f64) -> usize {
    (t / ENERGY_S).max(0.0) as usize
}

/// Move a splice to the quietest smoothed-energy frame within +/-SNAP_S.
/// argmin over the window, not the first frame under a threshold.
///
/// Each endpoint is snapped INDEPENDENTLY. An earlier version bounded each end
/// to its own half of the cut, to stop short candidates collapsing to zero
/// length — that was wrong: on a 5 s repeat both ends ran to the middle, where
/// the pause between takes is quietest, and a 5.00 s span became 0.17 s. It
/// shrank every span toward its centre.
///
/// The collapse case needs no guard: the duration test rejects it, and a
/// rejected alignment falls through to the next one. Very short candidates can
/// even come back with end < start; that fails `>= MIN_CUT_S` and falls through
/// too. Do not clamp.
fn snap_in(t: f64, energy: &[f64]) -> (f64, f64) {
    let a = frame(t - SNAP_S);
    let b = frame(t + SNAP_S).min(energy.len());
    if b <= a {
        let level = energy.get(frame(t)).copied().unwrap_or(0.0);
        return (t, level);
    }
    let mut best = a;
    for i in a..b {
        if energy[i] < energy[best] {
            best = i;
        }
    }
    (best as f64 * ENERGY_S, energy[best])
}

/// THE QUIET-RUN GUARD. Length of the contiguous quiet stretch containing `t`.
/// "Is this point quiet?" is satisfied mid-word by a stop consonant; measuring
/// how *long* the quiet lasts is what separates a real gap from a consonant.
fn quiet_run_s(t: f64, energy: &[f64]) -> f64 {
    let i = frame(t);
    if i >= energy.len() || energy[i] >= QUIET_DB {
        return 0.0;
    }
    let mut a = i;
    while a > 0 && energy[a - 1] < QUIET_DB {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < energy.len() && energy[b + 1] < QUIET_DB {
        b += 1;
    }
    (b - a + 1) as f64 * ENERGY_S
}

/// Length of the contiguous quiet stretch ending just before `t`.
///
/// Sourced from audio rather than from `filler.start - prev_word.end`, because
/// whisper.cpp's token timestamps tile the timeline: a pause is absorbed into
/// the previous word's end instead of left as a gap, so the token-derived gap
/// reads ~0 even where there is real silence. Same threshold, same intent.
/// Measured against GATE_DB, not QUIET_DB. The question here is "is there a
/// pause before this word" — speech / not-speech, which is GATE's job. QUIET is
/// the splice-safety guard, 6 dB stricter, and using it truncated real pauses:
/// the walk stops at the first frame above the level, so breath or room tone
/// inside a pause cut a 1 s gap down to 0.05 s. Measured, that put 33 words in
/// the 0.02-0.18 band where the reference has zero, and lost its 0.40 s+ tail.
fn quiet_before_s(t: f64, energy: &[f64]) -> f64 {
    let i = frame(t).min(energy.len());
    (i - quiet_start_frame(t, energy)) as f64 * ENERGY_S
}

/// First frame of the non-speech stretch that ends at `t`.
fn quiet_start_frame(t: f64, energy: &[f64]) -> usize {
    let i = frame(t).min(energy.len());
    let mut a = i;
    while a > 0 && energy[a - 1] < GATE_DB {
        a -= 1;
    }
    a
}

/// Where the previous speech actually stopped, from audio rather than from the
/// previous token's end. With tiled token timings `prev.end` sits right against
/// the filler's start, which collapses the span arithmetic; the audio knows
/// where the voice really stopped.
fn speech_end_before(t: f64, energy: &[f64]) -> f64 {
    quiet_start_frame(t, energy) as f64 * ENERGY_S
}

/// A cut may only land wholly inside a segment that plays at 1x — never inside
/// a sped-up stretch.
fn inside_1x(segs: &[Segment], s: f64, e: f64) -> bool {
    segs.iter()
        .any(|g| g.speed == 1.0 && g.src_start <= s && e <= g.src_end)
}

/// Propose cuts from the word list, then gate every one of them.
/// Returns (accepted, rejected, clipped) where `clipped` counts repeats the
/// rule found but for which no alignment could be spliced.
pub fn find_cuts(
    words: &[Word],
    samples: &[f32],
    sample_rate: u32,
    energy: &[f64],
    energy_edge: &[f64],
    segs: &[Segment],
) -> (Vec<Cut>, Vec<Rejected>, usize) {
    let tok: Vec<(String, f64, f64, String)> = words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| (norm(&w.text), w.start, w.end, w.text.trim().to_string()))
        .collect();

    let mut cands: Vec<Candidate> = Vec::new();

    // ---- stutters: identical adjacent words, keep the last -----------------
    let mut i = 0;
    while i + 1 < tok.len() {
        let mut j = i;
        while j + 1 < tok.len()
            && tok[j + 1].0 == tok[i].0
            && tok[j + 1].1 - tok[j].2 < STUTTER_GAP_S
        {
            j += 1;
        }
        if j > i {
            cands.push(Candidate {
                opts: vec![
                    (tok[i].1, tok[j].1, "keep-last"),
                    (tok[i].2, tok[j].2, "keep-last-alt"),
                ],
                kind: "stutter",
                text: tok[i].0.clone(),
                sim: 1.0,
                takes: j - i + 1,
            });
            i = j + 1;
        } else {
            i += 1;
        }
    }

    // ---- repeated phrases: keep only the LAST take -------------------------
    // Longest match wins: L runs downwards so a long phrase claims its span
    // before a shorter prefix of it can re-cut the same words.
    for l in (MIN_PHRASE..=7).rev() {
        if tok.len() < 2 * l {
            continue;
        }
        let mut i = 0;
        while i + 2 * l <= tok.len() {
            let phrase: Vec<&str> = (0..l).map(|k| tok[i + k].0.as_str()).collect();

            // Collect every take of the phrase. Three distinct cases, and they
            // must stay distinct: collapsing the last two into one `break` can
            // only ever record a single hit, which caps every chain at 2 takes.
            //
            // The window is anchored at `i` and spans SEARCH_WORDS tokens — not
            // re-anchored on each hit, which would let a chain walk arbitrarily
            // far. `last_end` advances to each accepted take, so the gap test
            // measures take-to-take rather than always from the first.
            let mut takes = vec![i];
            let mut last_end = tok[i + l - 1].2;
            let hi = (i + SEARCH_WORDS).min(tok.len() + 1 - l);

            for j in (i + l)..hi {
                if !(0..l).all(|k| tok[j + k].0 == phrase[k]) {
                    continue; // no match — keep scanning
                }
                if tok[j].1 - last_end > MAX_TAKE_GAP_S {
                    break; // match, but too far — the chain ends here
                }
                takes.push(j); // match, close enough — record and continue
                last_end = tok[j + l - 1].2;
            }

            if takes.len() >= 2 {
                let first = takes[0];
                let second = takes[1];
                let last = *takes.last().unwrap();

                // The four splice alignments, in preference order.
                // keep-last removes every take but the final one; the "-alt"
                // forms shift both splice points one phrase later, which is
                // often where the quiet actually is; the drop-* forms remove
                // only one copy — better than removing none.
                let opts = vec![
                    (tok[first].1, tok[last].1, "keep-last"),
                    (tok[first + l - 1].2, tok[last + l - 1].2, "keep-last-alt"),
                    (tok[first].1, tok[second].1, "drop-first"),
                    (tok[first + l - 1].2, tok[second + l - 1].2, "drop-second"),
                ];

                let (s, e) = (tok[first].1, tok[last].1);
                if !cands
                    .iter()
                    .any(|c| c.opts.iter().any(|o| !(e <= o.0 || s >= o.1)))
                {
                    let sim = similarity(
                        samples,
                        sample_rate,
                        tok[first].1,
                        tok[first + l - 1].2,
                        tok[last].1,
                        tok[last + l - 1].2,
                    );
                    cands.push(Candidate {
                        opts,
                        kind: "repeat",
                        text: phrase.join(" "),
                        sim,
                        takes: takes.len(),
                    });
                }
                i = last + l;
                continue;
            }
            i += 1;
        }
    }

    // ---- detachable fillers -------------------------------------------------
    // Seven discourse markers, not hesitation sounds — Whisper drops "um"/"uh".
    // The >= 0.18 s of silence before is what separates a discarded
    // sentence-opener from "and" used as a real conjunction mid-clause.
    for i in 1..tok.len().saturating_sub(1) {
        let (ref t, s, e_tok, ref raw) = tok[i];
        if !FILLER.contains(&t.as_str()) {
            continue;
        }
        // The SPAN is built from token boundaries, exactly as the reference
        // does. Deriving pe/ns from audio was wrong: it made the raw span cover
        // the whole silence on both sides, so snapping pulled both ends inward
        // and the cut collapsed or inverted. The reference builds a NARROW raw
        // span (measured mean 0.421 s) and lets snapping widen it outward into
        // the surrounding silence (accepted mean 0.671 s).
        //
        // Only the GAP TEST reads audio, because tiled token timings put the
        // pause inside the previous word's end rather than leaving a gap.
        let e = e_tok;
        // `pe` from audio, `ns`/`e` from tokens. The split is not arbitrary:
        //   max(pe + 0.14, s - 0.06)  -- a tiled pe sits at s, so pe + 0.14
        //     lands INSIDE the word and wins the max, inverting short fillers
        //     (measured raw min -0.040). From audio it sits in the real silence
        //     and loses the max, giving the reference's s - 0.06.
        //   min(ns - 0.02, e + 0.60)  -- from audio this reached the far side
        //     of the whole silence, making the raw span so wide that snapping
        //     pulled both ends inward and collapsed it.
        let prev_end = speech_end_before(s, energy_edge);
        let next_start = tok[i + 1].1;
        if quiet_before_s(s, energy_edge) < FILLER_GAP_S || e - s > FILLER_MAX_S {
            continue;
        }
        // The span is not the word: it eats up to 0.60 s of the trailing pause
        // (where most of the removed time comes from) and leaves 0.14 s of the
        // preceding gap and 0.02 s before the next word intact.
        cands.push(Candidate {
            opts: vec![(
                (prev_end + KEEP_GAP_S).max(s - 0.06),
                (next_start - 0.02).min(e + FILLER_TAIL_S),
                "span",
            )],
            kind: "filler",
            text: raw.clone(),
            sim: 1.0,
            takes: 1,
        });
    }

    // ---- snap + validate ---------------------------------------------------
    // Gate order matches the reference: duration -> splice quietness ->
    // quiet-run guard -> spacing. Cheapest rejection first.
    cands.sort_by(|a, b| a.at().partial_cmp(&b.at()).unwrap());

    let mut accepted: Vec<Cut> = Vec::new();
    let mut rejected: Vec<Rejected> = Vec::new();
    let mut clipped = 0usize;

    for c in cands {
        // the two takes must actually sound alike, so matching words from a
        // mis-transcription cannot trigger a cut
        if c.kind == "repeat" && c.sim < SIM_MIN {
            rejected.push(Rejected {
                start: c.at(),
                kind: c.kind,
                why: format!("similarity {:.2}", c.sim),
            });
            continue;
        }

        let mut placed = false;
        let mut last_why = String::from("no alignment");

        for &(a, b, label) in &c.opts {
            let (s, qs) = snap_in(a, energy);
            let (e, qe) = snap_in(b, energy);
            let dur = e - s;

            if !(MIN_CUT_S..=MAX_CUT_S).contains(&dur) {
                last_why = format!("duration {dur:.2}s");
                continue;
            }
            if qs.max(qe) > QUIET_DB {
                last_why = format!("splice {:.0}dB", qs.max(qe));
                continue;
            }
            let (rs, re) = (quiet_run_s(s, energy), quiet_run_s(e, energy));
            if rs < QUIET_RUN_S || re < QUIET_RUN_S {
                last_why = format!("quiet run {rs:.2}s/{re:.2}s");
                continue;
            }
            if accepted.last().is_some_and(|k| s < k.end + CUT_SPACING_S) {
                last_why = String::from("overlap");
                continue;
            }
            if !inside_1x(segs, s, e) {
                last_why = String::from("not in a 1x segment");
                continue;
            }

            accepted.push(Cut {
                start: s,
                end: e,
                kind: c.kind,
                text: c.text.clone(),
                sim: c.sim,
                takes: c.takes,
                align: label,
            });
            placed = true;
            break;
        }

        if !placed {
            // the rule found it but the audio would not allow the cut
            if c.kind == "repeat" {
                clipped += 1;
            }
            rejected.push(Rejected {
                start: c.at(),
                kind: c.kind,
                why: last_why,
            });
        }
    }

    (accepted, rejected, clipped)
}

/// Subtract accepted cuts from the 1x segments. Sped segments pass through
/// untouched — we never cut inside a speed-up.
pub fn apply_cuts(segs: Vec<Segment>, cuts: &[Cut]) -> Vec<Segment> {
    let mut out = Vec::new();

    for g in segs {
        if g.speed != 1.0 {
            out.push(g);
            continue;
        }
        let mut cursor = g.src_start;
        for c in cuts
            .iter()
            .filter(|c| c.start >= g.src_start && c.end <= g.src_end)
        {
            if c.start > cursor {
                out.push(Segment {
                    src_start: cursor,
                    src_end: c.start,
                    speed: 1.0,
                });
            }
            cursor = c.end;
        }
        if cursor < g.src_end {
            out.push(Segment {
                src_start: cursor,
                src_end: g.src_end,
                speed: 1.0,
            });
        }
    }

    out
}
