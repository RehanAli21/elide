use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Deserialize, Debug)]
pub struct Loudnorm {
    pub input_i: String,
    pub input_tp: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Analysis {
    pub gain_db: f64,
    pub input_i: f64,
    pub input_tp: f64,
    pub sample_rate: u32,
    pub samples: usize,
    pub grid_len: usize,
    pub src_duration_s: f64,
}

pub fn measure_loudness(input: &str, target_lufs: f64, prefilter: &[&str]) -> Result<(f64, f64)> {
    let mut parts: Vec<String> = prefilter.iter().map(|s| s.to_string()).collect();
    parts.push(format!("loudnorm=I={target_lufs}:print_format=json"));
    let filter = parts.join(",");
    let out = Command::new("ffmpeg")
        .args(["-i", input, "-af", &filter, "-f", "null", "-"])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to extract audio: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let start = stderr.find('{').context("no JSON in loudnorm output")?;
    let end = stderr.rfind('}').context("no JSON in loudnorm output")?;
    let block = &stderr[start..=end];
    let parsed: Loudnorm = serde_json::from_str(block).context("could not parse loudnorm JSON")?;

    let input_i: f64 = parsed.input_i.parse().context("bad input_i")?;
    let input_tp: f64 = parsed.input_tp.parse().context("bad input_tp")?;

    Ok((input_i, input_tp))
}

pub fn extract_audio(input: &str, wav_path: &Path, gain_db: f64) -> Result<()> {
    let volume = format!("volume={gain_db}dB");

    let out = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            input,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-af",
            &volume,
            "-c:a",
            "pcm_f32le",
        ])
        .arg(wav_path)
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to extract audio: {}", stderr.trim());
    }

    Ok(())
}
