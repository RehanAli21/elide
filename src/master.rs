use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::constants::{COMPRESSOR, HIGHPASS_HZ, LIMITER_CEILING, LIMITER_COMP, MASTER_TP};

/// `target_lufs` is policy — the prompt picks it within -23..-14. Everything
/// else here (the true-peak ceiling, the limiter) is fixed.
pub fn master(input: &Path, out: &Path, compress: bool, target_lufs: f64) -> Result<()> {
    let mut parts = vec![
        format!("highpass=f={HIGHPASS_HZ}"),
        "afftdn=nr=12:nf=-45".to_string(),
    ];
    if compress {
        parts.push(COMPRESSOR.to_string());
    }

    // SINGLE-PASS, dynamic. Do NOT feed measured_* back in and do NOT set
    // linear=true. Linear mode computes one fixed gain and applies it flat with
    // no limiting, so the peak lands wherever the gain puts it and the AAC
    // encode then adds on top with nothing holding it back — measured drift to
    // -1.05 dBTP against a -1.2 limit. Dynamic mode keeps loudnorm's true-peak
    // limiter active, which is what actually enforces the ceiling. The
    // reference delivers -1.35/-1.40 dBTP this way.
    //
    // LRA=11 is a ceiling, not a target — the reference delivers 6.10 and 2.80.
    let ask = target_lufs + LIMITER_COMP;
    parts.push(format!("loudnorm=I={ask}:TP={MASTER_TP}:LRA=11"));

    // Hard ceiling into the AAC encode. loudnorm keeps the reference's TP=-1.5
    // so integrated loudness matches; the limiter alone owns the peak.
    parts.push(format!(
        "alimiter=limit={LIMITER_CEILING}:level=disabled"
    ));
    let filter = parts.join(",");

    let o = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(input)
        .args([
            "-c:v",
            "copy",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-af",
            &filter,
            "-c:a",
            "pcm_s16le",
        ])
        .arg(out)
        .output()
        .context("could not run ffmpeg")?;

    if !o.status.success() {
        bail!(
            "master failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(())
}

pub fn finalize(concat_path: &Path, out_path: &Path) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(concat_path)
        .args([
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ])
        .arg(out_path)
        .output()
        .context("could not run ffmpeg")?;

    if !out.status.success() {
        bail!(
            "Finalize failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}
