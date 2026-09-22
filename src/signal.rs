//! M12 — signal B, the per-genre "nothing is happening" mask.
//!
//! This is the ONE thing that is per-genre. Everything downstream — the plan,
//! the time map, rendering, mastering, captions, verification — is
//! genre-independent and works unchanged. Adding a genre means writing one
//! implementation of this trait and nothing else.
//!
//! The decision is `dead = (not speaking) AND (nothing happening)`. So a genre
//! with no signal B must return a mask that is always TRUE, collapsing the rule
//! to silence-only. Returning all-FALSE would mean nothing is ever dead and the
//! tool would silently do nothing. The sign is easy to get backwards.
//!
//! The trait arrives now, at M12, and not earlier: with one implementation
//! there was nothing to generalise over, and a plain function was correct.

use anyhow::{bail, Result};

use crate::constants::{GRID_S, SAMPLE_FRAMES};
use crate::crop::sample_frames;
use crate::freeze::{detect_freezes, paint_freezes};

/// A boolean mask on the plan grid: true where nothing is happening.
pub trait DeadTimeSignal {
    fn name(&self) -> &'static str;

    /// `crop` is the content region, where the genre has one.
    fn mask(&self, input: &str, crop: &str, grid_len: usize, duration: f64) -> Result<Vec<bool>>;
}

/// Screen recordings: the picture is frozen. The original, unchanged.
pub struct Freeze;

impl DeadTimeSignal for Freeze {
    fn name(&self) -> &'static str {
        "freeze"
    }

    fn mask(&self, input: &str, crop: &str, grid_len: usize, duration: f64) -> Result<Vec<bool>> {
        let freezes = match detect_freezes(input, crop) {
            Ok(f) => f,
            Err(e) => return Err(e.context("freeze detection failed")),
        };
        let total: f64 = freezes
            .iter()
            .map(|&(s, e)| {
                // open = the video ends while frozen (only the last can be)
                let end = match e {
                    Some(t) => t,
                    None => duration,
                };
                end - s
            })
            .sum();
        println!(
            "freezes     {} blocks, {total:.1} s frozen ({:.1}%)",
            freezes.len(),
            100.0 * total / duration
        );
        Ok(paint_freezes(&freezes, grid_len, duration))
    }
}

/// Slide lectures: freeze detection inverted.
///
/// A slide is a still image, so "static picture" is true almost everywhere and
/// tells you nothing. What carries the lecture is the slide *change*. So the
/// mask is true everywhere EXCEPT for a short window after each change, which
/// is the only moment something is happening.
pub struct Slides;

/// Mean absolute frame-to-frame difference above which a slide has changed.
/// Deliberately high: a slide transition is a whole-frame event, unlike the
/// small local motion a screen recording shows.
const SLIDE_CHANGE_DIFF: f64 = 6.0;

/// How long a slide change counts as "something happening" afterwards.
const SLIDE_ACTIVE_S: f64 = 2.0;

impl DeadTimeSignal for Slides {
    fn name(&self) -> &'static str {
        "slides"
    }

    fn mask(&self, input: &str, _crop: &str, grid_len: usize, duration: f64) -> Result<Vec<bool>> {
        // Whole frame, not the content crop: a slide fills the frame, and there
        // is no app window to isolate.
        let frames = match sample_frames(input, duration) {
            Ok(f) => f,
            Err(e) => return Err(e.context("slide detection could not sample frames")),
        };
        if frames.len() < 2 {
            bail!("slide detection needs at least 2 sampled frames");
        }

        let step = duration / SAMPLE_FRAMES as f64;
        let mut mask = vec![true; grid_len];
        let mut changes = 0;

        for i in 1..frames.len() {
            let (a, b) = (&frames[i - 1], &frames[i]);
            let n = a.len().min(b.len());
            if n == 0 {
                continue;
            }
            let diff: f64 = (0..n)
                .map(|k| (a[k] as f64 - b[k] as f64).abs())
                .sum::<f64>()
                / n as f64;

            if diff >= SLIDE_CHANGE_DIFF {
                changes += 1;
                // something is happening from this frame for SLIDE_ACTIVE_S
                let t = i as f64 * step;
                let lo = ((t / GRID_S) as usize).min(grid_len);
                let hi = (((t + SLIDE_ACTIVE_S) / GRID_S) as usize).min(grid_len);
                for slot in mask.iter_mut().take(hi).skip(lo) {
                    *slot = false;
                }
            }
        }

        let still = mask.iter().filter(|&&b| b).count();
        println!(
            "slides      {changes} changes, {:.1}% of grid static",
            100.0 * still as f64 / grid_len as f64
        );
        Ok(mask)
    }
}

/// Talking head to camera: no signal B exists.
///
/// Returns ALL TRUE, which collapses the decision to silence-only — the
/// ordinary audio-based behaviour. All-false would mean nothing is ever dead.
pub struct None_;

impl DeadTimeSignal for None_ {
    fn name(&self) -> &'static str {
        "none"
    }

    fn mask(&self, _input: &str, _crop: &str, grid_len: usize, _duration: f64) -> Result<Vec<bool>> {
        println!("signal none  all-true mask, dead air decided by silence alone");
        Ok(vec![true; grid_len])
    }
}

/// Pick the implementation by name.
pub fn for_name(name: &str) -> Result<Box<dyn DeadTimeSignal>> {
    match name {
        "freeze" => Ok(Box::new(Freeze)),
        "slides" => Ok(Box::new(Slides)),
        "none" => Ok(Box::new(None_)),
        other => bail!("unknown --signal {other} (expected freeze, slides or none)"),
    }
}
