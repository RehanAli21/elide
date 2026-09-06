//! The one rule that keeps the model non-load-bearing: **core must not import
//! the AI layer.**
//!
//! Without this the boundary is a convention, and conventions erode. With it,
//! the claim "the video is byte-identical whether or not a model is present" is
//! checked by the build rather than asserted in a document.

use std::fs;
use std::path::Path;

/// Every module that decides what the edit *is*. None of these may reach for a
/// model, directly or transitively.
const CORE: [&str; 15] = [
    "align.rs",
    "audio.rs",
    "captions.rs",
    "cli.rs",
    "constants.rs",
    "crop.rs",
    "disfluency.rs",
    "dsp.rs",
    "features.rs",
    "freeze.rs",
    "master.rs",
    "plan.rs",
    "probe.rs",
    "render.rs",
    "signal.rs",
];

#[test]
fn core_modules_do_not_import_ai() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();

    for name in CORE {
        let path = src.join(name);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => panic!("core module {name} is missing — update this list if it moved"),
        };

        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("crate::ai") || code.contains("use ai::") {
                offenders.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "core must not import the AI layer:\n{}",
        offenders.join("\n")
    );
}

/// verify.rs decides whether the export is allowed. If a model could reach it,
/// a model could pass a broken export.
#[test]
fn verification_does_not_import_ai() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("verify.rs"),
    )
    .expect("verify.rs");

    for line in text.lines() {
        let code = line.split("//").next().unwrap_or("");
        assert!(
            !code.contains("crate::ai"),
            "verification must never depend on a model: {}",
            line.trim()
        );
    }
}
