//! M13 — the AI layer.
//!
//! THE DETERMINISTIC PIPELINE DECIDES. THE MODEL ONLY PROPOSES, INSIDE A CLOSED
//! ACTION SPACE, BEHIND A DETERMINISTIC GATE.
//!
//! Three laws:
//!
//! 1. Never let the model decide anything you can measure. Cut points, speed
//!    factors, timings, loudness, splice positions — all arithmetic.
//! 2. Convert generation into selection, one decision per call, always with
//!    "0 = keep as-is" so abstaining is expressible.
//! 3. Gate every output deterministically and ignore self-reported confidence.
//!
//! **No module under `core` may import this one.** The rule is enforced by a
//! test (`tests/core_has_no_ai.rs`), and it is the single thing that keeps the
//! model non-load-bearing: the video is byte-identical whether this layer runs
//! or not, and only text artefacts improve.
//!
//! `monitor` lives here rather than in core because it is an AI task
//! (AI_ARCHITECTURE section 8.6, `pipeline.monitor`) — even though what it
//! adjusts is decided by a deterministic score, not by the model.

pub mod capabilities;
pub mod gates;
pub mod monitor;
pub mod provider;
pub mod tasks;
