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

/// Energy analysis window. Frames hop by ENERGY_S but each covers this much
/// audio, so consecutive frames overlap by 22 ms. Without the overlap the
/// energy array is spiky and the edge guard fires on transients.
pub const ENERGY_WIN_S: f64 = 0.032;

/// Energy frame hop. Slice i covers energy frames 2i and 2i+1.
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

/// Delivery target for the finished programme (M6), and what verification
/// checks against. A delivery target, not a measurement reference.
pub const MASTER_LUFS: f64 = -14.0;

/// What we ask loudnorm for, compensating the limiter's measured cost.
///
/// The limiter's -2.0 dBFS ceiling sits below loudnorm's -1.5 TP target, so
/// every peak loudnorm places gets pulled down and the two fight continuously.
/// Measured cost: 0.05 LUFS, the SAME on two very different edits (demo
/// -13.89 -> -13.94, extension -14.16 -> -14.21), so it is a stable offset
/// rather than content-dependent. Asking for -13.95 lands both back on the
/// reference's own delivered values.
///
/// Two files agreeing is suggestive, not proof. If a third file shows an offset
/// other than ~0.05, compensation is the wrong tool and the two stages need
/// decoupling properly instead.
pub const LOUDNORM_I: f64 = -13.95;

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
/// How far the edge guard may search from a run boundary.
/// Bounded so a run that is loud throughout gets trimmed, not deleted.
pub const MAX_SHRINK_S: f64 = 3.0;

/// Smoothing window for the EDGE GUARD only — 100 ms, 10 frames.
/// Deliberately wider than the 30 ms used by the splice test and quiet-run
/// guard: dead air is full of mouse clicks, and a 10 ms click 20 dB over gate
/// lifts a 100 ms mean by only ~2 dB, so it cannot trip the guard. Sustained
/// speech still crosses easily.
pub const EDGE_SMOOTH_S: f64 = 0.10;

/// Extra backoff past the last loud frame, so a cut is not adjacent to speech.
pub const EDGE_MARGIN_S: f64 = 0.15;

/// A dead run may never be trimmed below this. Also the planner's floor.
pub const MIN_DEAD_S: f64 = 1.0;

/// Dead runs closer than this are merged before the length rules apply.
pub const DEAD_BRIDGE_S: f64 = 2.0;

/// ffmpeg's atempo filter accepts 0.5–2.0 only; higher speeds chain instances.
pub const ATEMPO_MAX: f64 = 2.0;

/// Removes rumble below speech. The reference uses 75, not 80.
pub const HIGHPASS_HZ: u32 = 75;

/// True-peak ceiling for the DELIVERED programme. Verification allows this
/// +0.3 dB (= -1.2) because AAC is the last thing to touch the signal.
pub const MASTER_TP: f64 = -1.5;

/// Hard ceiling held into the AAC encode, as linear amplitude: -2.0 dBFS.
///
/// A DELIBERATE DIVERGENCE from v6, which has no limiter. Justified by
/// measurement: with identical settings the delivered true peak came out -1.37
/// on one edit and -0.98 on another, against a -1.2 limit — the ceiling was
/// luck-of-content, and a verification harness whose outcome depends on the
/// edit is not a harness.
///
/// Lowering loudnorm's own TP instead was tried and rejected: it cost ~0.09
/// LUFS because the limiter engaged on sustained content, pushing the extension
/// to -14.24. Splitting the jobs keeps loudnorm at the reference's TP=-1.5
/// (which delivers the reference's own loudness) and gives the peak ceiling to
/// a dedicated limiter.
///
/// `level=disabled` is essential: alimiter applies auto level compensation by
/// default, which would restore the gain and defeat the whole thing.
pub const LIMITER_CEILING: f64 = 0.794;

/// Applied only when flat gain to MASTER_LUFS would exceed MASTER_TP.
/// makeup=8 supplies most of the gain so loudnorm's limiter isn't doing the work.
pub const COMPRESSOR: &str = "acompressor=threshold=-28dB:ratio=3:attack=10:release=250:makeup=8";

// ---------------------------------------------------------------------------
// Disfluency removal (M11) — cutting inside speech
// ---------------------------------------------------------------------------

/// Shortest cut worth making. Below this the splice risk buys nothing.
pub const MIN_CUT_S: f64 = 0.15;

/// Longest disfluency cut — ONE cap for every kind, no per-kind limit.
/// Under keep-last a repeat span covers every abandoned take and the material
/// between them, so multi-second cuts are expected, not suspicious.
pub const MAX_CUT_S: f64 = 9.5;

/// Silence deliberately left behind at a splice so words do not butt together.
pub const KEEP_GAP_S: f64 = 0.14;

/// A filler is detachable only with at least this much space before it.
/// Otherwise removing it would slam the previous word into the next.
pub const FILLER_GAP_S: f64 = 0.18;

/// Longest a word may be and still count as a filler.
pub const FILLER_MAX_S: f64 = 0.85;

/// How much of the pause after a filler may go with it. Not the whole pause —
/// long gaps belong to the dead-air logic.
pub const FILLER_TAIL_S: f64 = 0.60;

/// Repeated adjacent words further apart than this are not a stutter.
pub const STUTTER_GAP_S: f64 = 0.9;

/// Shortest repeated phrase worth cutting, in words. The match loop runs
/// 7,6,5,4 — never 3. v3 used 3; v6 raised the floor to 4 because short matches
/// recur constantly in ordinary speech: at 3 this let through "all the data",
/// "it will all", "which i will", "from instagram and".
pub const MIN_PHRASE: usize = 4;

/// Consecutive takes of a repeat must be this close, or the span between them
/// is real content rather than a restart. v6 widened this from 0.6.
pub const MAX_TAKE_GAP_S: f64 = 3.0;

/// Spectral correlation required to believe two takes are the same phrase.
pub const SIM_MIN: f64 = 0.55;

/// How far ahead (in words) to look for the next take of a phrase. Bounds the
/// search; the 3.0 s chain break decides whether a found take still counts.
/// Different jobs — not competing proxies.
pub const SEARCH_WORDS: usize = 40;

/// Accepted cuts must be at least this far apart, not merely non-overlapping.
pub const CUT_SPACING_S: f64 = 0.08;

