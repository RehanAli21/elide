use anyhow::{Result, bail};
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
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow::Error::from(e).context("could not run ffprobe. Is ffmpeg installed and on PATH?"));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffprobe failed on {input}: {}", stderr.trim());
    }

    match String::from_utf8(out.stdout) {
        Ok(s) => Ok(s),
        Err(e) => Err(anyhow::Error::from(e).context("ffprobe returned invalid UTF-8")),
    }
}

/// "60000/1001" -> 59.94. None if the text is not a readable fraction; the
/// caller turns that into an error with its own message.
pub fn parse_fps(s: &str) -> Option<f64> {
    let (num, den) = match s.split_once('/') {
        Some(pair) => pair,
        None => return None,
    };
    let num: f64 = match num.parse() {
        Ok(v) => v,
        Err(_) => return None,
    };
    let den: f64 = match den.parse() {
        Ok(v) => v,
        Err(_) => return None,
    };
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}
