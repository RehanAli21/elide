use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Deserialize, Debug)]
pub struct Loudnorm {
    pub input_i: String,
    pub input_tp: String,
    pub input_lra: String,
    pub input_thresh: String,
    pub target_offset: String,
}

/// All five numbers loudnorm's first pass reports, parsed to f64.
/// The second pass needs every one of them to normalise accurately.
pub struct LoudnormStats {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
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

/// First pass of two-pass loudnorm: measure the file *through* `prefilter`
/// (the filters that run before loudnorm, e.g. highpass/afftdn/compressor)
/// and return all five values the second pass will feed back.
pub fn measure_loudnorm(
    input: &str,
    prefilter: &[&str],
    target_i: f64,
    target_tp: f64,
    target_lra: f64,
) -> Result<LoudnormStats> {
    let mut parts: Vec<String> = prefilter.iter().map(|s| s.to_string()).collect();
    parts.push(format!(
        "loudnorm=I={target_i}:TP={target_tp}:LRA={target_lra}:print_format=json"
    ));
    let filter = parts.join(",");
    let out = Command::new("ffmpeg")
        .args(["-i", input, "-af", &filter, "-f", "null", "-"])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("loudnorm measurement failed: {}", stderr.trim());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let start = stderr.find('{').context("no JSON in loudnorm output")?;
    let end = stderr.rfind('}').context("no JSON in loudnorm output")?;
    let parsed: Loudnorm =
        serde_json::from_str(&stderr[start..=end]).context("could not parse loudnorm JSON")?;

    Ok(LoudnormStats {
        input_i: parsed.input_i.parse().context("bad input_i")?,
        input_tp: parsed.input_tp.parse().context("bad input_tp")?,
        input_lra: parsed.input_lra.parse().context("bad input_lra")?,
        input_thresh: parsed.input_thresh.parse().context("bad input_thresh")?,
        target_offset: parsed.target_offset.parse().context("bad target_offset")?,
    })
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
