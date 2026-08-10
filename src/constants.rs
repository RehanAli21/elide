//! Project-wide constants.
//!
//! POLICY values may be nudged by the monitor loop or set by the prompt.
//! GUARDS are invariants — they make unsafe cuts unavailable, and nothing
//! outside this file may widen them (CLI_AND_PROMPT.md §1).

// ---------------------------------------------------------------------------
// Rule A — the time grid
// ---------------------------------------------------------------------------

/// One grid slice, in seconds. Every mask indexes on this.
/// Changing it invalidates every threshold expressed in slices.
pub const GRID_S: f64 = 0.02;

/// Energy frame length. Slice i covers energy frames 2i and 2i+1.
/// Finer than the grid because QUIET_RUN_S needs more than 5 samples to judge.
pub const ENERGY_S: f64 = 0.01;

/// Analysis audio rate.
/// Required by Silero and Whisper; not a property of any input file.
pub const SAMPLE_RATE: u32 = 16000;

/// Samples per grid slice. Integer division by this defines grid_len.
/// The trailing partial slice is dropped, so the grid ends ~40 ms before the
/// video — grid_len and src_duration_s are separate values, never substituted.
pub const SAMPLES_PER_SLICE: usize = 320;

// ---------------------------------------------------------------------------
// Content region detection (M3)
// ---------------------------------------------------------------------------

/// Downscaled frame width for activity measurement.
pub const GRID_W: usize = 160;

/// Downscaled frame height for activity measurement.
pub const GRID_H: usize = 90;

/// Frames sampled across the whole file; fps = this / duration.
/// Fixed count, so the gap between sampled frames is duration-dependent — and
/// frame-to-frame activity scales with that gap.
pub const SAMPLE_FRAMES: usize = 200;

/// Below this peak activity, no content region exists at all.
/// Measured maxima: static 0.73, extension 4.6, demo 42.1, talking-head 51.9.
/// Absolute, and everything absolute here has eventually needed rescaling.
pub const ACTIVITY_MIN: f32 = 2.0;

/// A cell is content above this fraction of the file's own max.
/// Relative on purpose: an absolute 0.5 floor silently clipped the mask on
/// densely-sampled files and shattered one content region into 125 fragments.
pub const ACTIVITY_RATIO: f32 = 0.08;

/// Mean pixel difference above which a blob counts as moving in a frame.
/// The blob's mean across its cells, not a count of cells that changed.
pub const BUSY_DIFF: f64 = 0.5;

/// Blobs smaller than this are ignored.
/// Separates the taskbar clock (21 cells) from a webcam bubble (326).
pub const MIN_BLOB: usize = 50;

/// Above this fraction of frames moving, a blob is a camera, not content.
/// Measured: app 33.7%, clock 91.5%, webcam 100%, talking-head 100%. Nothing
/// observed between 34% and 91%, so the boundary is wide but unmapped.
pub const MAX_BUSY: f64 = 0.90;

// ---------------------------------------------------------------------------
// Speech mask (M2)
// ---------------------------------------------------------------------------

/// Silero window size, in samples. Fixed by the model, not configurable.
/// 512/16000 = 32 ms, which does not divide the 20 ms grid; slice i takes the
/// chunk containing its midpoint sample.
pub const VAD_CHUNK: usize = 512;

/// POLICY. Probability at which speech starts. The monitor may nudge this.
pub const VAD_ENTER: f32 = 0.30;

/// Probability below which speech ends.
/// The gap from ENTER stops a mid-sentence dip splitting one run into two:
/// 2416 transitions without it, 1638 with. Floor at 0.01 if ENTER is swept.
pub const VAD_EXIT: f32 = 0.15;

/// Gaps shorter than this become speech.
/// Raw VAD excludes the pauses between words; without this, hundreds of fake
/// micro-silences.
pub const BRIDGE_GAP_S: f64 = 0.35;

/// Speech runs shorter than this are dropped as blips.
/// Keyboard clicks, chair creaks, half-caught breaths.
pub const MIN_SPEECH_S: f64 = 0.25;

/// Speech run extended backwards by this.
/// So a later cut never lands on a consonant onset.
pub const PAD_BEFORE_S: f64 = 0.50;

/// Speech run extended forwards by this.
/// Longer than PAD_BEFORE_S because speech trails off rather than stopping.
pub const PAD_AFTER_S: f64 = 0.55;

// ---------------------------------------------------------------------------
// Freeze detection (M3)
// ---------------------------------------------------------------------------

/// freezedetect noise floor for calling two frames identical.
pub const FREEZE_NOISE_DB: &str = "-58dB";

/// Minimum stillness before a freeze block is reported.
pub const FREEZE_MIN_S: f64 = 2.0;

/// Sampling rate for freeze detection.
/// Timestamps land on 0.2 s boundaries; block edges are no finer than that.
pub const FREEZE_FPS: u32 = 5;

// ---------------------------------------------------------------------------
// Loudness
// ---------------------------------------------------------------------------

/// Analysis copy is normalised here before any dB threshold applies.
/// Source loudness spans 12.2 dB across four videos, so an absolute threshold
/// meant 21.8 dB below programme on one file and 34.0 dB on another.
pub const TARGET_LUFS: f64 = -23.0;

/// Delivery target for the finished programme (M6).
/// A delivery target, not a measurement reference. Never merge the two.
pub const MASTER_LUFS: f64 = -14.0;

// ---------------------------------------------------------------------------
// GUARDS — fixed in code, forever (CLI_AND_PROMPT.md §1)
//
// The disfluency score rewards removing more seconds. It is honest only
// because these make damaging cuts unavailable. Not parameters.
// ---------------------------------------------------------------------------

/// Both splice points must be quieter than this.
/// -21.78 dB relative to programme, shifted by TARGET_LUFS.
pub const QUIET_DB: f64 = TARGET_LUFS - 21.78;

/// Speech-level energy. Dead-air run boundaries are trimmed back from it.
/// -15.78 dB relative to programme, shifted by TARGET_LUFS.
pub const GATE_DB: f64 = TARGET_LUFS - 15.78;

/// Minimum contiguous quiet stretch around a splice point.
/// Separates a real gap from a stop consonant, which "is this point quiet?"
/// alone does not. Must be measured from audio, not word spans.
pub const QUIET_RUN_S: f64 = 0.10;

/// How far a splice may move to find a quiet point.
/// Widening this clipped words before QUIET_RUN_S existed. Do not widen it
/// without that guard in place.
pub const SNAP_S: f64 = 0.35;
