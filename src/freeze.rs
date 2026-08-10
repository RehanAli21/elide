use anyhow::{bail, Context, Result};
use std::process::Command;

pub fn detect_freezes(input: &str, crop: &str) -> Result<Vec<(f64, Option<f64>)>> {
    let filter = format!("crop={crop},fps=5,freezedetect=n=-58dB:d=2.0");

    let out = Command::new("ffmpeg")
        .args(["-i", input, "-vf", &filter, "-an", "-f", "null", "-"])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("freezedetect failed: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut freezes: Vec<(f64, Option<f64>)> = Vec::new();

    for line in stderr.lines() {
        if let Some(v) = line.split("freeze_start: ").nth(1) {
            if let Ok(t) = v.trim().parse::<f64>() {
                freezes.push((t, None));
            }
        } else if let Some(v) = line.split("freeze_end: ").nth(1) 
            && let Ok(t) = v.trim().parse::<f64>() 
            && let Some(last) = freezes.last_mut() 
        {
            last.1 = Some(t); 
        }
    }

    Ok(freezes)
}

pub fn paint_freezes(freezes: &[(f64, Option<f64>)], grid_len: usize, duration: f64) -> Vec<bool> {
    let mut frozen = vec![false; grid_len];

    for &(start, end) in freezes {
        let end = end.unwrap_or(duration);
        let i0 = ((start / 0.02) as usize).min(grid_len - 1);
        let i1 = ((end / 0.02) as usize).min(grid_len);

        frozen[i0..i1].fill(true);
    }

    frozen
}
