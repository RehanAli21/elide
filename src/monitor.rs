//! M12b — the monitor loop.
//!
//! THE MODEL PROPOSES. THE MEASUREMENT DECIDES.
//!
//! The model is not a judge and never sees a pass/fail question. It gets one
//! step's diagnostic, the current parameter values, and one sentence of causal
//! guidance, and it answers with a single parameter name and a direction. The
//! code then re-runs the step with that parameter nudged, scores BOTH results
//! deterministically, and keeps whichever scores higher. A wrong proposal costs
//! one extra run and is thrown away by the score — which is how a model
//! measured at precision 0.50 stays useful.
//!
//! Asking it to judge instead does not work, and this was measured: given the
//! verification table it either replied OK to a corrupted a/v sync of 0.412, or,
//! when told the healthy ranges, false-alarmed on clean data and called
//! -1.91 dBTP "above the maximum allowed -1.2" — the sign backwards. It cannot
//! do threshold arithmetic. It does not have to.
//!
//! Only a step with BOTH an automatic quality score and an adjustable parameter
//! can be monitored. Verification has neither, so it is not monitored.
//!
//! GUARDS ARE NOT ADJUSTABLE. The reference lets the model nudge SNAP and
//! QUIET; this project does not — they are fixed in code forever, so the only
//! parameter accepted here is the VAD threshold, which constants.rs marks
//! POLICY. The caller enforces that, not the model.
//!
//! If ollama is unreachable the result is ok=true, indistinguishable from
//! approval, and the pipeline runs on its defaults.

use std::process::Command;

use serde::Deserialize;

use crate::captions::Word;
use crate::constants::GRID_S;

const MODEL: &str = "qwen2.5:7b";
const URL: &str = "http://localhost:11434/api/generate";
const TIMEOUT_S: &str = "60";

#[derive(Deserialize, Debug)]
pub struct Decision {
    pub ok: bool,
    pub parameter: String,
    pub direction: String,
    pub reason: String,
}

impl Decision {
    fn approve(reason: &str) -> Self {
        Decision {
            ok: true,
            parameter: "none".to_string(),
            direction: "none".to_string(),
            reason: reason.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct GenResponse {
    response: String,
}

/// Ask for one parameter and a direction. Never a verdict.
///
/// `guide` is one sentence of CAUSAL direction — which way the parameter moves
/// behaviour — not a threshold and not an expected range.
pub fn review(step: &str, diag: &str, params: &str, guide: &str) -> Decision {
    let prompt = format!(
        "Monitoring one step of a video editor.\n\nSTEP: {step}\n\nDIAGNOSTIC:\n{diag}\n\n\
         PARAMETERS: {params}\n\n{guide}\n\nIf acceptable: ok=true, parameter=\"none\", \
         direction=\"none\". Otherwise name ONE parameter and a direction.\nReply JSON."
    );

    // A hard enum on `direction` means the model cannot emit anything else.
    let body = serde_json::json!({
        "model": MODEL,
        "prompt": prompt,
        "stream": false,
        "options": { "temperature": 0, "num_predict": 200 },
        "format": {
            "type": "object",
            "properties": {
                "ok":        { "type": "boolean" },
                "parameter": { "type": "string" },
                "direction": { "type": "string", "enum": ["increase", "decrease", "none"] },
                "reason":    { "type": "string" }
            },
            "required": ["ok", "parameter", "direction", "reason"]
        }
    })
    .to_string();

    let out = match Command::new("curl")
        .args([
            "-s",
            "-m",
            TIMEOUT_S,
            "-X",
            "POST",
            URL,
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Decision::approve("model unavailable"),
    };

    let envelope: GenResponse = match serde_json::from_slice(&out.stdout) {
        Ok(g) => g,
        Err(_) => return Decision::approve("model unavailable (bad envelope)"),
    };

    match serde_json::from_str(&envelope.response) {
        Ok(d) => d,
        Err(_) => Decision::approve("model unavailable (bad JSON)"),
    }
}

/// THE JUDGE. Word coverage minus the false-alarm rate outside words.
///
/// Deterministic, and the only thing allowed to decide whether a proposed
/// change is kept. Higher is better: 1.0 would mean every spoken slice is
/// marked speech and no silent slice is.
pub fn speech_score(mask: &[bool], words: &[Word], grid_len: usize) -> f64 {
    let mut is_word = vec![false; grid_len];
    for w in words {
        let a = ((w.start / GRID_S) as usize).min(grid_len);
        let b = ((w.end / GRID_S) as usize).min(grid_len);
        for slot in is_word.iter_mut().take(b).skip(a) {
            *slot = true;
        }
    }

    let (mut word_n, mut word_hit, mut gap_n, mut gap_hit) = (0usize, 0usize, 0usize, 0usize);
    for i in 0..grid_len {
        if is_word[i] {
            word_n += 1;
            if mask[i] {
                word_hit += 1;
            }
        } else {
            gap_n += 1;
            if mask[i] {
                gap_hit += 1;
            }
        }
    }

    let coverage = if word_n > 0 {
        word_hit as f64 / word_n as f64
    } else {
        0.0
    };
    let false_alarm = if gap_n > 0 {
        gap_hit as f64 / gap_n as f64
    } else {
        0.0
    };
    coverage - false_alarm
}
