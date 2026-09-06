//! M11 prerequisite — word-level transcript from Whisper, chunked.
//!
//! Whisper collapses repeated phrases when it decodes a long file in one pass
//! (its sliding window judges the repeat redundant), which erases exactly what
//! disfluency removal hunts for. The fix is independent chunks: decode ~30 s at
//! a time with no memory of neighbours, and trim the overlap by keeping a word
//! only if its midpoint falls in the chunk's own region.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::captions::Word;

const CHUNK_S: f64 = 30.0;
const OVERLAP_S: f64 = 3.0;
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

/// Ensure `models/ggml-base.bin` exists (create the dir, download if missing),
/// and return its path.
pub fn ensure_model() -> Result<PathBuf> {
    let dir = Path::new("models");
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    let path = dir.join("ggml-base.bin");
    if !path.exists() {
        println!("downloading whisper model (base, ~148MB)...");
        let status = Command::new("curl")
            .args(["-L", "-f", "-o"])
            .arg(&path)
            .arg(MODEL_URL)
            .status()
            .context("could not run curl to download the model")?;
        if !status.success() {
            bail!("whisper model download failed");
        }
    }
    Ok(path)
}

/// Transcribe 16 kHz mono `samples` into word-level entries with real timings,
/// decoding in independent overlapping chunks so repeats survive.
pub fn transcribe_chunked(samples: &[f32], sample_rate: u32, model: &Path) -> Result<Vec<Word>> {
    // route whisper.cpp's chatter into the log crate; no logger is installed,
    // so it is dropped instead of flooding stdout
    whisper_rs::install_logging_hooks();

    let ctx = WhisperContext::new_with_params(
        model.to_str().context("non-UTF-8 model path")?,
        WhisperContextParameters::default(),
    )
    .context("could not load whisper model")?;
    let mut state = ctx.create_state().context("could not create whisper state")?;

    let threads = std::thread::available_parallelism()
        .map(|x| x.get() as i32)
        .unwrap_or(4);

    let sr = sample_rate as f64;
    let dur = samples.len() as f64 / sr;
    let step = CHUNK_S - OVERLAP_S;

    let mut out: Vec<Word> = Vec::new();
    let mut cs = 0.0;
    let mut ci = 0;

    while cs < dur {
        let ce = (cs + CHUNK_S).min(dur);
        let a = (cs * sr) as usize;
        let b = ((ce * sr) as usize).min(samples.len());
        let seg = &samples[a..b];
        if (seg.len() as f64) < sr * 0.5 {
            break; // too short to be worth decoding
        }

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_n_threads(threads);
        params.set_token_timestamps(true);
        params.set_translate(false);
        params.set_no_context(true); // decode each chunk in isolation
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, seg)
            .with_context(|| format!("whisper failed on chunk at {cs:.0}s"))?;

        // a word belongs to this chunk if its midpoint is in the chunk's own
        // region (overlap halves are owned by the neighbour)
        let lo = if ci == 0 { cs } else { cs + OVERLAP_S / 2.0 };
        let hi = if ce >= dur { ce } else { ce - OVERLAP_S / 2.0 };

        for w in chunk_words(&state, cs)? {
            let mid = (w.start + w.end) / 2.0;
            if lo <= mid && mid < hi {
                out.push(w);
            }
        }

        if ci % 8 == 0 {
            println!("  chunk {} ({cs:.0}s)  {} words", ci + 1, out.len());
        }
        cs += step;
        ci += 1;
    }

    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    Ok(out)
}

/// Group the tokens of one decoded chunk into words. `cs` is the chunk's start
/// time in the full file; token timestamps are relative to the chunk (in
/// centiseconds), so we add `cs`. A token beginning with a space starts a new
/// word (Whisper's convention).
fn chunk_words(state: &whisper_rs::WhisperState, cs: f64) -> Result<Vec<Word>> {
    let mut words: Vec<Word> = Vec::new();

    let mut text = String::new();
    let mut start = 0.0;
    let mut end = 0.0;
    let mut psum = 0.0f32;
    let mut n = 0u32;
    let mut open = false;

    let mut flush = |text: &mut String, start: f64, end: f64, psum: f32, n: u32| {
        if !text.trim().is_empty() {
            words.push(Word {
                text: text.clone(),
                start,
                end,
                prob: if n > 0 { (psum / n as f32) as f64 } else { 0.0 },
            });
        }
        text.clear();
    };

    let n_segments = state.full_n_segments();
    for si in 0..n_segments {
        let seg = match state.get_segment(si) {
            Some(s) => s,
            None => continue,
        };
        for ti in 0..seg.n_tokens() {
            let tok = match seg.get_token(ti) {
                Some(t) => t,
                None => continue,
            };
            let piece = tok.to_str_lossy().unwrap_or_default().to_string();
            if piece.starts_with('[') {
                continue; // special token, e.g. [_BEG_]
            }
            let data = tok.token_data();
            let a = cs + data.t0 as f64 / 100.0;
            let b = cs + data.t1 as f64 / 100.0;
            let p = tok.token_probability();

            if piece.starts_with(' ') && open {
                flush(&mut text, start, end, psum, n);
                open = false;
            }
            if !open {
                start = a;
                psum = 0.0;
                n = 0;
                open = true;
            }
            text.push_str(&piece);
            end = b;
            psum += p;
            n += 1;
        }
    }
    if open {
        flush(&mut text, start, end, psum, n);
    }

    Ok(words)
}
