//! **The prompt sets policy. The prompt never touches a guard.**
//!
//! Policy is how the edit should feel. Guards are what make a cut safe. The
//! reason this is a test and not a comment: the disfluency score rewards
//! removing more seconds, and it is only honest because the quiet-run guard
//! makes damaging cuts unavailable. A prompt that could widen a guard could
//! talk the pipeline into destroying the audio and would score itself higher
//! for doing it.

use std::fs;
use std::path::Path;

/// The four. Fixed in code, forever.
const GUARDS: [&str; 4] = ["QUIET_DB", "GATE_DB", "QUIET_RUN_S", "SNAP_S"];

fn src(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name))
        .unwrap_or_else(|_| panic!("{name} is missing — update this test if it moved"))
}

#[test]
fn policy_never_names_a_guard() {
    let text = src("policy.rs");

    for (n, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        for g in GUARDS {
            assert!(
                !code.contains(g),
                "policy.rs:{} reaches a guard ({g}): {}",
                n + 1,
                line.trim()
            );
        }
    }
}

/// The planner takes `Pacing` from policy. It must carry pacing only — the
/// moment a guard becomes a field, the prompt can set it.
#[test]
fn pacing_carries_no_guard() {
    let text = src("plan.rs");

    let start = text
        .find("pub struct Pacing")
        .expect("plan.rs no longer defines Pacing — this test needs updating");
    let body = &text[start..start + text[start..].find('}').expect("unclosed Pacing")];

    for g in GUARDS {
        assert!(
            !body.contains(g),
            "Pacing exposes a guard ({g}) to the prompt:\n{body}"
        );
    }
}

/// Every numeric field the prompt can set is bounded, and the bound is applied
/// in one place. If `clamp` stops covering a field, an unbounded number reaches
/// ffmpeg.
#[test]
fn every_numeric_policy_field_is_clamped() {
    let text = src("policy.rs");

    let start = text.find("fn clamp").expect("policy.rs has no clamp");
    let body = &text[start..start + text[start..].find("\n    }").expect("unclosed clamp")];

    for field in ["max_speed", "target_lufs"] {
        assert!(
            body.contains(&format!("self.{field}")),
            "{field} is settable from the prompt but never clamped:\n{body}"
        );
    }
}
