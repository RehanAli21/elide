use anyhow::{bail, Result};
use std::process::Command;

/// Read freezedetect's report into (start, end) pairs. Only the LAST freeze may
/// be left open — that is a video which ends while frozen.
///
/// Every malformed line is now a hard error. It used to be skipped in silence,
/// and silence here was dangerous: a `freeze_end` that failed to parse left its
/// freeze open, and `paint_freezes` paints an open freeze to the end of the
/// file — so everything after that point became "frozen", i.e. dead air to be
/// cut, with no error anywhere. A report we cannot read is a reason to stop,
/// not to guess.
pub fn detect_freezes(input: &str, crop: &str) -> Result<Vec<(f64, Option<f64>)>> {
    let filter = format!("crop={crop},fps=5,freezedetect=n=-58dB:d=2.0");

    let out = match Command::new("ffmpeg")
        .args(["-i", input, "-vf", &filter, "-an", "-f", "null", "-"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow::Error::from(e).context("could not run ffmpeg. Is it installed and on PATH?"));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("freezedetect failed: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut freezes: Vec<(f64, Option<f64>)> = Vec::new();

    for line in stderr.lines() {
        if let Some(v) = line.split("freeze_start: ").nth(1) {
            let t = match v.trim().parse::<f64>() {
                Ok(t) => t,
                Err(e) => bail!("freezedetect printed an unreadable freeze_start {v:?}: {e}"),
            };
            // A new freeze while the previous one is still open means ffmpeg
            // skipped an end. The open one would be painted to end-of-file.
            match freezes.last() {
                Some(&(prev, None)) => {
                    bail!("freezedetect started a freeze at {t:.2}s while the one at {prev:.2}s never ended")
                }
                _ => freezes.push((t, None)),
            }
        } else if let Some(v) = line.split("freeze_end: ").nth(1) {
            let t = match v.trim().parse::<f64>() {
                Ok(t) => t,
                Err(e) => bail!("freezedetect printed an unreadable freeze_end {v:?}: {e}"),
            };
            match freezes.last_mut() {
                Some(last) => match last.1 {
                    None => last.1 = Some(t),
                    Some(done) => {
                        bail!("freezedetect ended a freeze at {t:.2}s that already ended at {done:.2}s")
                    }
                },
                None => bail!("freezedetect ended a freeze at {t:.2}s that never started"),
            }
        }
    }

    Ok(freezes)
}

pub fn paint_freezes(freezes: &[(f64, Option<f64>)], grid_len: usize, duration: f64) -> Vec<bool> {
    let mut frozen = vec![false; grid_len];

    for &(start, end) in freezes {
        // Open means the video ends while frozen. detect_freezes guarantees
        // only the last freeze can be open, so this is never a guess.
        let end = match end {
            Some(e) => e,
            None => duration,
        };
        let i0 = ((start / 0.02) as usize).min(grid_len - 1);
        let i1 = ((end / 0.02) as usize).min(grid_len);

        frozen[i0..i1].fill(true);
    }

    frozen
}
