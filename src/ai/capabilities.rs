//! The capability probe. Run once per model, then cached.
//!
//! This is what makes "works with any model" true rather than aspirational.
//! Everything here is MEASURED — a model with a 32k advertised window is often
//! unreliable well before it, and a model card never states selection accuracy.
//!
//! Tiering is on PRECISION rather than coverage: abstaining is free because the
//! gate falls back, while acting wrongly is what costs.

use std::time::Instant;

use serde::Deserialize;
use serde_json::json;

use super::provider::{Capabilities, LlmProvider, Tier};

#[derive(Deserialize)]
struct Echo {
    value: i64,
}

#[derive(Deserialize)]
struct Choice {
    index: i64,
}

/// Selection items with known answers. Each asks the model to pick an index,
/// and index 0 is always "leave it alone" so abstaining is expressible.
const SELECTION: [(&str, &str, i64); 6] = [
    (
        "The speaker said a word that sounded like 'division' while the screen shows a field labelled Duration.",
        "0 = keep as heard, 1 = duration, 2 = schedules",
        1,
    ),
    (
        "The speaker said 'blockboard' while the screen shows a button labelled Block.",
        "0 = keep as heard, 1 = block, 2 = keyboard",
        1,
    ),
    (
        "The speaker said 'twenty twenty' and no related word appears on screen.",
        "0 = keep as heard, 1 = BrainClean, 2 = schedules",
        0,
    ),
    (
        "A phrase repeats: 'I have added some images' then 'I have added some images so that'.",
        "0 = not a repeat, 1 = a restart worth cutting",
        1,
    ),
    (
        "A phrase repeats introduced by 'Second,' in a list of options.",
        "0 = a list, leave it, 1 = a restart worth cutting",
        0,
    ),
    (
        "The recording is a screen capture of a phone emulator with a webcam overlay.",
        "0 = event footage, 1 = app demo, 2 = musical performance",
        1,
    ),
];

/// Probe a provider and return what it can actually be trusted with.
pub fn probe(p: &dyn LlmProvider) -> Capabilities {
    // ---- JSON compliance: echo a known integer under a schema ---------------
    let echo_schema = json!({
        "type": "object",
        "properties": { "value": { "type": "integer" } },
        "required": ["value"]
    });
    let mut json_ok = 0;
    for n in 1..=5 {
        let out = p.complete(
            &format!("Reply with JSON where value is exactly {n}."),
            Some(&echo_schema),
            50,
            0.0,
        );
        if let Ok(text) = out {
            if let Ok(e) = serde_json::from_str::<Echo>(&text) {
                if e.value == n {
                    json_ok += 1;
                }
            }
        }
    }
    let json_mode = json_ok >= 4;

    // If it cannot even echo, it is a Null provider or an unreachable one.
    if json_ok == 0 {
        return Capabilities {
            name: p.name().to_string(),
            json_mode: false,
            tok_per_sec: 0.0,
            selection: 0.0,
            generation: 0.0,
            tier: Tier::None,
        };
    }

    // ---- selection accuracy against known answers ---------------------------
    let choice_schema = json!({
        "type": "object",
        "properties": { "index": { "type": "integer" } },
        "required": ["index"]
    });
    let mut hits = 0;
    for (situation, options, want) in SELECTION {
        let prompt = format!(
            "Pick exactly one option by index.\n\nSITUATION: {situation}\n\nOPTIONS: {options}\n\nReply JSON."
        );
        if let Ok(text) = p.complete(&prompt, Some(&choice_schema), 50, 0.0) {
            if let Ok(c) = serde_json::from_str::<Choice>(&text) {
                if c.index == want {
                    hits += 1;
                }
            }
        }
    }
    let selection = hits as f64 / SELECTION.len() as f64;

    // ---- generation accuracy: the REAL task, not a toy ----------------------
    // These must be the job we would actually hand it — repairing a mangled
    // caption line. An earlier version asked it to echo single words, scored
    // 4/4, and promoted the model a whole tier on nothing. A probe that
    // flatters the model is worse than no probe: it converts "I don't know"
    // into a confident wrong answer, the same failure as a tuner that cannot
    // tell it is blind.
    //
    // Every item is a real ASR error from this recording. The last one is
    // expected to FAIL for any model without the on-screen vocabulary — it is
    // in the set precisely so the score cannot reach 1.0 by luck.
    // NOTE the answers are NOT in the prompts. An earlier version wrote "the
    // screen shows a field labelled Duration" and then checked whether the
    // reply contained "duration" — the model only had to copy the question
    // back, and scored 4/4. That is the on-screen-evidence case, which is a
    // different and much easier task than generation.
    let gen_items = [
        (
            "Fix this line from an automatic transcript of a phone app demo. Reply with only \
             the corrected line.\n\nwhat the division does is block the apps until that division has passed",
            "duration",
        ),
        (
            "Fix this line from an automatic transcript of a phone app demo. Reply with only \
             the corrected line.\n\nlets say we are working on a leading task where you read a book",
            "reading",
        ),
        (
            "Fix this line from an automatic transcript about eye health. Reply with only the \
             corrected line.\n\nthis is the 2020 2022 rule so you look away for twenty seconds",
            "20-20-20",
        ),
        (
            "Fix this line from an automatic transcript of a phone app demo. Reply with only \
             the corrected line.\n\nlet me demonstrate my application powerpoint which blocks distracting apps",
            "brainclean",
        ),
    ];
    let mut gen_hits = 0;
    for (prompt, want) in gen_items {
        if let Ok(text) = p.complete(prompt, None, 120, 0.0) {
            if text.to_lowercase().contains(want) {
                gen_hits += 1;
            }
        }
    }
    let generation = gen_hits as f64 / gen_items.len() as f64;

    // ---- throughput ---------------------------------------------------------
    let t0 = Instant::now();
    let produced = p
        .complete("Count from 1 to 60, separated by spaces.", None, 200, 0.0)
        .map(|t| t.split_whitespace().count())
        .unwrap_or(0);
    let secs = t0.elapsed().as_secs_f64().max(1e-6);
    let tok_per_sec = produced as f64 / secs;

    Capabilities {
        name: p.name().to_string(),
        json_mode,
        tok_per_sec,
        selection,
        generation,
        tier: Tier::from_scores(selection, generation),
    }
}

/// How many scored items back the tier. The documented minimum before a tier
/// assignment is trustworthy is 30+; below that it is provisional and must say
/// so, because a 6-item probe sits one item away from the next threshold.
pub const PROBE_ITEMS: usize = SELECTION.len() + 4;
pub const PROBE_ITEMS_TRUSTED: usize = 30;

/// The tool never silently degrades — it says what it will and will not do.
pub fn describe(c: &Capabilities) -> String {
    let caveat = if PROBE_ITEMS < PROBE_ITEMS_TRUSTED {
        format!(
            " (tier PROVISIONAL — {PROBE_ITEMS} probe items, {PROBE_ITEMS_TRUSTED}+ needed \
             to trust it)"
        )
    } else {
        String::new()
    };
    describe_tier(c) + &caveat
}

fn describe_tier(c: &Capabilities) -> String {
    match c.tier {
        Tier::None => format!(
            "{} — no usable model. Captions stay raw ASR, chapters are \"Section N\". \
             The edit is unaffected.",
            c.name
        ),
        Tier::Small => format!(
            "{} — selection enabled with strict gates, generation disabled. Caption \
             corrections are proposed for review, not applied.",
            c.name
        ),
        Tier::Mid => format!(
            "{} — selection and classification enabled, generation disabled.",
            c.name
        ),
        Tier::Large => format!("{} — all tasks enabled including generation.", c.name),
    }
}
