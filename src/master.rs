use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::audio::measure_loudnorm;
use crate::constants::{COMPRESSOR, HIGHPASS_HZ, MASTER_LUFS, MASTER_TP};

pub fn master(input: &Path, out: &Path, compress: bool) -> Result<()> {
    // The filters that run before loudnorm. loudnorm must be measured through
    // exactly these, because this is the audio it will actually see.
    let mut prefix = vec![
        format!("highpass=f={HIGHPASS_HZ}"),
        "afftdn=nr=12:nf=-45".to_string(),
    ];
    if compress {
        prefix.push(COMPRESSOR.to_string());
    }

    // Pass 1: measure the file as loudnorm will see it.
    let prefix_refs: Vec<&str> = prefix.iter().map(|s| s.as_str()).collect();
    let stats = measure_loudnorm(
        input.to_str().context("non-UTF-8 path")?,
        &prefix_refs,
        MASTER_LUFS,
        MASTER_TP,
        11.0,
    )?;

    // Pass 2: apply, feeding the measured values back so loudnorm normalises
    // from full knowledge of the file instead of a live guess.
    let loud = format!(
        "loudnorm=I={MASTER_LUFS}:TP={MASTER_TP}:LRA=11:\
measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear=true",
        stats.input_i, stats.input_tp, stats.input_lra, stats.input_thresh, stats.target_offset
    );
    let mut parts = prefix;
    parts.push(loud);
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
