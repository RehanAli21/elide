//! M9 — captions. Turn a word-level transcript into an SRT that stays in sync
//! with the edit: group words into cues, push every timing through the plan's
//! source->output map, break lines on sentence/clause boundaries, share time by
//! character count, and guarantee each cue is on screen long enough to read.
//!
//! The text is whatever the transcript says — correcting rough ASR against
//! on-screen evidence is the AI layer (M13), not this. What M9 guarantees is
//! timing and structure.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::plan::{map_to_output, Plan};

/// One word from the transcript JSON (source-timed).
#[derive(Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub prob: f64,
}

const MAXC: usize = 84; // logical line length used when splitting a long cue
const WRAP: usize = 44; // display wrap width (SRT convention)
const MAX_LINES: usize = 2; // display lines per cue
const CPS: f64 = 18.0; // reading speed, characters per second
const MIN_CUE_S: f64 = 0.4; // floor on how short a cue may be

/// character count (not bytes) — widths are measured in characters
fn clen(s: &str) -> usize {
    s.chars().count()
}

struct Cue {
    start: f64,
    end: f64,
    text: String,
}

/// Read the transcript, build the SRT, write it to `out_path`. Returns the
/// number of cues written. `scale` = measured output duration / planned, so the
/// cues stretch to match the real file (the render runs a touch long because
/// sped segments round to whole frames).
pub fn write_srt(words: &[Word], plan: &Plan, scale: f64, out_path: &Path) -> Result<usize> {
    let out_dur = plan.out_duration_s * scale;

    // 1. group words into cues at sentence ends (with a length safety valve)
    let mut cues = group_cues(words);

    // 2. re-time each cue through the edit (scaled to the real file), floor length
    for c in &mut cues {
        c.start = map_to_output(plan, c.start) * scale;
        c.end = map_to_output(plan, c.end) * scale;
        if c.end - c.start < MIN_CUE_S {
            c.end = c.start + MIN_CUE_S;
        }
    }

    // 3. sort and remove any overlaps introduced by the mapping
    cues.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    for i in 0..cues.len().saturating_sub(1) {
        if cues[i].end > cues[i + 1].start {
            cues[i].end = (cues[i].start + MIN_CUE_S).max(cues[i + 1].start - 0.05);
        }
    }

    // 4. split each cue into <=MAXC fragments, sharing its time by char count
    let mut frags: Vec<(f64, f64, String)> = Vec::new();
    for c in &cues {
        let chunks = split_pieces(&c.text);
        let total: usize = chunks.iter().map(|s| clen(s)).sum::<usize>().max(1);
        let span = c.end - c.start;
        let mut at = c.start;
        for ch in chunks {
            let d = span * clen(&ch) as f64 / total as f64;
            let end = (at + d).min(c.end);
            frags.push((at, end, ch));
            at += d;
        }
    }

    // 5. give every fragment long enough to read, borrowing from the gap ahead
    for i in 0..frags.len() {
        let need = clen(&frags[i].2) as f64 / CPS;
        if frags[i].1 - frags[i].0 < need {
            let limit = if i + 1 < frags.len() {
                frags[i + 1].0
            } else {
                out_dur
            };
            frags[i].1 = (frags[i].0 + need).min(limit);
        }
    }

    // 6. format SRT
    let mut out = String::new();
    for (i, (s, e, t)) in frags.iter().enumerate() {
        let body = wrap(t, WRAP)
            .into_iter()
            .take(MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(&format!("{}\n{} --> {}\n{body}\n\n", i + 1, ts(*s), ts(*e)));
    }

    fs::write(out_path, out)
        .with_context(|| format!("could not write {}", out_path.display()))?;

    Ok(frags.len())
}

/// Group words into cues, flushing at sentence-ending punctuation or when a cue
/// grows unreasonably long without any.
fn group_cues(words: &[Word]) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut buf = String::new();
    let mut start = 0.0;
    let mut end = 0.0;
    let mut open = false;

    for w in words {
        let t = w.text.trim();
        if t.is_empty() {
            continue;
        }
        if !open {
            start = w.start;
            open = true;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(t);
        end = w.end;

        let ends_sentence = t.ends_with('.') || t.ends_with('!') || t.ends_with('?');
        if ends_sentence || clen(&buf) >= MAXC * 3 {
            cues.push(Cue {
                start,
                end,
                text: buf.trim().to_string(),
            });
            buf.clear();
            open = false;
        }
    }
    if open && !buf.trim().is_empty() {
        cues.push(Cue {
            start,
            end,
            text: buf.trim().to_string(),
        });
    }
    cues
}

/// Break a cue's text into readable chunks of <= MAXC characters: sentence
/// boundaries first, then clause punctuation, then clause keywords, then a hard
/// word wrap. Mirrors the reference `pieces`.
fn split_pieces(t: &str) -> Vec<String> {
    let t = t.trim();
    if clen(t) <= MAXC {
        return vec![t.to_string()];
    }

    for delims in [&['.', '?', '!'][..], &[',', ';', ':', '—'][..]] {
        let parts = split_after(t, delims);
        if parts.len() > 1 {
            return merge(parts.iter().flat_map(|p| split_pieces(p)).collect());
        }
    }

    let parts = split_before_keywords(t);
    if parts.len() > 1 {
        return merge(parts.iter().flat_map(|p| split_pieces(p)).collect());
    }

    wrap(t, MAXC)
}

/// Split after any delimiter char that is followed by whitespace, keeping the
/// delimiter on the left piece.
fn split_after(t: &str, delims: &[char]) -> Vec<String> {
    let chars: Vec<char> = t.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();

    for i in 0..chars.len() {
        cur.push(chars[i]);
        let boundary =
            delims.contains(&chars[i]) && i + 1 < chars.len() && chars[i + 1].is_whitespace();
        if boundary {
            let p = cur.trim().to_string();
            if !p.is_empty() {
                parts.push(p);
            }
            cur.clear();
        }
    }
    let p = cur.trim().to_string();
    if !p.is_empty() {
        parts.push(p);
    }
    parts
}

/// Split before a clause-leading keyword (and/but/so/…), so the keyword starts
/// the next piece. Mirrors the reference's keyword lookahead split.
fn split_before_keywords(t: &str) -> Vec<String> {
    const KW: [&str; 8] = ["and", "but", "so", "because", "which", "that", "then", "or"];
    let words: Vec<&str> = t.split_whitespace().collect();
    let mut parts = Vec::new();
    let mut cur: Vec<&str> = Vec::new();

    for (i, w) in words.iter().enumerate() {
        let bare = w
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if i > 0 && !cur.is_empty() && KW.contains(&bare.as_str()) {
            parts.push(cur.join(" "));
            cur.clear();
        }
        cur.push(w);
    }
    if !cur.is_empty() {
        parts.push(cur.join(" "));
    }
    parts
}

/// Re-join adjacent fragments that still fit within MAXC together.
fn merge(parts: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if clen(last) + 1 + clen(p) <= MAXC {
                last.push(' ');
                last.push_str(p);
                continue;
            }
        }
        out.push(p.to_string());
    }
    out
}

/// Greedy word wrap to `width` characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();

    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if clen(&cur) + 1 + clen(word) <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Seconds -> "HH:MM:SS,mmm" (SRT timestamp).
fn ts(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    format!("{h:02}:{m:02}:{s:02},{:03}", ms % 1000)
}
