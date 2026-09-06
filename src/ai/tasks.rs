//! The AI tasks. Each one is a single decision inside a closed action space,
//! behind a deterministic gate, with a fallback that runs when the model is
//! absent or its answer fails the gate.

use serde::Deserialize;
use serde_json::json;

use super::provider::{Capabilities, LlmProvider};

#[derive(Deserialize)]
struct Ok_ {
    ok: bool,
}

/// `caption.intelligible` — classify. The safest task in the system.
///
/// Never edits anything. It produces the "please proofread these lines" list,
/// so a wrong answer costs nothing: a missed line stays as it was, and a
/// falsely flagged line is read by a human who disagrees. Advisory by
/// construction, which is why it is built first.
///
/// Fallback: flag nothing.
pub fn intelligible(p: &dyn LlmProvider, caps: &Capabilities, line: &str) -> bool {
    if !caps.tier.allows_selection() {
        return true; // no model: flag nothing
    }
    let schema = json!({
        "type": "object",
        "properties": { "ok": { "type": "boolean" } },
        "required": ["ok"]
    });
    let prompt = format!(
        "This is one line of an automatic transcript of a spoken software demo. \
         The speaker is not a native English speaker.\n\nLINE: {line}\n\n\
         Is this line intelligible as written — would a viewer understand it? \
         ok=true if yes, ok=false if it reads as garbled or nonsensical.\nReply JSON."
    );

    match p.complete(&prompt, Some(&schema), 50, 0.0) {
        Ok(text) => serde_json::from_str::<Ok_>(&text).map(|r| r.ok).unwrap_or(true),
        Err(_) => true, // unreachable: flag nothing
    }
}

/// Run the proofread pass over caption lines and return those worth a human
/// look. Triage first: only lines that look risky are sent, because at several
/// seconds a call, sending all of them costs twenty minutes and most are fine.
pub fn proofread_list(
    p: &dyn LlmProvider,
    caps: &Capabilities,
    lines: &[String],
) -> Vec<String> {
    lines
        .iter()
        .filter(|l| worth_checking(l))
        .filter(|l| !intelligible(p, caps, l))
        .cloned()
        .collect()
}

/// Deterministic triage. DSP proposes, the model adjudicates only contested
/// cases — this is what makes a slow local model affordable.
fn worth_checking(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    // A line with several very short tokens in a row is usually where the ASR
    // fell apart on accented speech.
    let stubby = words.iter().filter(|w| w.trim().len() <= 2).count();
    stubby * 3 >= words.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::{Null, Tier};

    fn null_caps() -> Capabilities {
        Capabilities {
            name: "null".into(),
            json_mode: false,
            tok_per_sec: 0.0,
            selection: 0.0,
            generation: 0.0,
            tier: Tier::None,
        }
    }

    #[test]
    fn null_provider_flags_nothing() {
        let lines = vec![
            "a b c d e f".to_string(),
            "this line is perfectly fine".to_string(),
        ];
        assert!(proofread_list(&Null, &null_caps(), &lines).is_empty());
    }

    #[test]
    fn triage_skips_healthy_lines() {
        assert!(!worth_checking("this line is perfectly fine and readable"));
        assert!(worth_checking("so it is a to be of my"));
    }
}
