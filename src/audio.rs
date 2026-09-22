use anyhow::{anyhow, bail, Result};
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

/// Run ffmpeg with `args`. Fails with `what` if ffmpeg cannot be started or
/// exits non-zero. Returns stderr, which is where ffmpeg writes its reports.
fn run_ffmpeg(args: &[&str], what: &str) -> Result<String> {
    let out = match Command::new("ffmpeg").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow::Error::from(e).context("could not run ffmpeg. Is it installed and on PATH?"));
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        bail!("{what}: {}", stderr.trim());
    }
    Ok(stderr)
}

/// Pull loudnorm's JSON report out of ffmpeg's stderr.
fn loudnorm_json(stderr: &str) -> Result<Loudnorm> {
    let start = match stderr.find('{') {
        Some(i) => i,
        None => return Err(anyhow!("no JSON in loudnorm output")),
    };
    let end = match stderr.rfind('}') {
        Some(i) => i,
        None => return Err(anyhow!("no JSON in loudnorm output")),
    };
    match serde_json::from_str(&stderr[start..=end]) {
        Ok(v) => Ok(v),
        Err(e) => Err(anyhow::Error::from(e).context("could not parse loudnorm JSON")),
    }
}

/// loudnorm reports every number as a string. `name` is used in the message.
fn number(value: &str, name: &str) -> Result<f64> {
    match value.parse::<f64>() {
        Ok(v) => Ok(v),
        Err(e) => Err(anyhow::Error::from(e).context(format!("bad {name}: {value:?}"))),
    }
}

pub fn measure_loudness(input: &str, target_lufs: f64, prefilter: &[&str]) -> Result<(f64, f64)> {
    let mut parts: Vec<String> = prefilter.iter().map(|s| s.to_string()).collect();
    parts.push(format!("loudnorm=I={target_lufs}:print_format=json"));
    let filter = parts.join(",");

    let stderr = match run_ffmpeg(
        &["-i", input, "-af", &filter, "-f", "null", "-"],
        "loudness measurement failed",
    ) {
        Ok(s) => s,
        Err(e) => return Err(e),
    };
    let parsed = match loudnorm_json(&stderr) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };

    let input_i = match number(&parsed.input_i, "input_i") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let input_tp = match number(&parsed.input_tp, "input_tp") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };

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

    let stderr = match run_ffmpeg(
        &["-i", input, "-af", &filter, "-f", "null", "-"],
        "loudnorm measurement failed",
    ) {
        Ok(s) => s,
        Err(e) => return Err(e),
    };
    let parsed = match loudnorm_json(&stderr) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };

    let input_i = match number(&parsed.input_i, "input_i") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let input_tp = match number(&parsed.input_tp, "input_tp") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let input_lra = match number(&parsed.input_lra, "input_lra") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let input_thresh = match number(&parsed.input_thresh, "input_thresh") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };
    let target_offset = match number(&parsed.target_offset, "target_offset") {
        Ok(v) => v,
        Err(e) => return Err(e),
    };

    Ok(LoudnormStats {
        input_i,
        input_tp,
        input_lra,
        input_thresh,
        target_offset,
    })
}

pub fn extract_audio(input: &str, wav_path: &Path, gain_db: f64) -> Result<()> {
    let volume = format!("volume={gain_db}dB");

    let out = match Command::new("ffmpeg")
        .args([
            "-y", "-i", input, "-vn", "-ac", "1", "-ar", "16000", "-af", &volume, "-c:a",
            "pcm_f32le",
        ])
        .arg(wav_path)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Err(anyhow::Error::from(e).context("could not run ffmpeg. Is it installed and on PATH?"));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to extract audio: {}", stderr.trim());
    }

    Ok(())
}
