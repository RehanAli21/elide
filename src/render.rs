use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::constants::ATEMPO_MAX;
use crate::plan::Segment;

fn atempo_chain(speed: f64) -> String {
    let mut parts = Vec::new();
    let mut r = speed;

    while r > ATEMPO_MAX {
        parts.push(ATEMPO_MAX);
        r /= ATEMPO_MAX;
    }
    parts.push(r);
    parts
        .iter()
        .map(|p| format!("atempo={p:.6}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn render_segments(input: &str, seg: &Segment, out_path: &Path) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");

    cmd.args([
        "-y",
        "-ss",
        &format!("{:.6}", seg.src_start),
        "-to",
        &format!("{:.6}", seg.src_end),
        "-i",
        input,
    ]);

    if seg.speed != 1.0 {
        cmd.args(["-vf", &format!("setpts=PTS/{:.6}", seg.speed)]);
        cmd.args(["-af", &format!("{},volume=0", atempo_chain(seg.speed))]);
    }

    cmd.args([
        "-c:v",
        "libx264",
        "-preset",
        "fast",
        "-crf",
        "19",
        "-profile:v",
        "high",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "120",
        "-c:a",
        "pcm_s16le",
        "-ar",
        "48000",
        "-ac",
        "2",
    ]);
    cmd.arg(out_path);

    let out = cmd.output().context("could not run ffmpeg")?;
    if !out.status.success() {
        bail!(
            "ffmpeg failed on segment: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn concat_segments(list_path: &Path, out_path: &Path) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(list_path)
        .args(["-c", "copy"])
        .arg(out_path)
        .output()
        .context("could not run ffmpeg")?;

    if !out.status.success() {
        bail!(
            "concat failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}
