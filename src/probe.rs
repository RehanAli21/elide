use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

#[derive(Deserialize, Debug)]
pub struct Probe {
    pub format: Format,
    pub streams: Vec<Stream>,
}

#[derive(Deserialize, Debug)]
pub struct Format {
    pub duration: String,
}

#[derive(Deserialize, Debug)]
pub struct Stream {
    pub codec_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub r_frame_rate: Option<String>,
    pub sample_rate: Option<String>,
}

pub fn ffprobe_json(input: &str) -> Result<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(input)
        .output()
        .context("could not run ffprobe. Is fffmpeg installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffprobe failed on {input}: {}", stderr.trim());
    }

    String::from_utf8(out.stdout).context("ffprobe returned invalid UTF-8")
}

pub fn parse_fps(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}
