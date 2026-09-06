use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use hound::WavReader;

use crate::audio::measure_loudness;
use crate::constants::{MASTER_LUFS, MASTER_TP};
use crate::plan::{map_to_source, Plan};
use crate::probe::{ffprobe_json, Probe};

#[derive(Debug)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

// ---------------------------------------------------------------- 5. faststart

fn check_faststart(out_path: &Path) -> Result<Check> {
    // read the first 64 KB and look for moov before mdat
    let bytes = fs::read(out_path).context("could not read output")?;
    let head = &bytes[..bytes.len().min(65536)];

    let moov = find_atom(head, b"moov");
    let mdat = find_atom(head, b"mdat");

    let passed = match (moov, mdat) {
        (Some(m), Some(d)) => m < d,
        (Some(_), None) => true, // moov early, mdat past the window
        _ => false,
    };

    Ok(Check {
        name: "faststart",
        passed,
        detail: format!("moov at {moov:?}, mdat at {mdat:?}"),
    })
}

fn find_atom(bytes: &[u8], tag: &[u8; 4]) -> Option<usize> {
    bytes.windows(4).position(|w| w == tag)
}

// ----------------------------------------------------------------- 4. loudness

fn check_loudness(out_path: &Path) -> Result<Check> {
    let (i, tp) = measure_loudness(
        out_path.to_str().context("non-UTF-8 path")?,
        MASTER_LUFS,
        &[],
    )?;

    // +/-0.2 LUFS per BUILD_STEPS M7's acceptance criterion. It was 0.1 here,
    // taken from M6's prose describing a typical range rather than the
    // criterion — and 0.1 rejects the reference's own deliverables:
    //   demonstration_v6  -13.92  dev 0.08  passes
    //   extension_v6      -14.17  dev 0.17  FAILS
    // A threshold that rejects the artifact it was written to describe is wrong.
    // Single-pass dynamic loudnorm lands across a ~0.25 LUFS range in practice.
    //
    // The true-peak limit stays where it is: AAC is the last thing to touch the
    // signal, and the reference clears -1.2 with 0.15-0.20 dB to spare.
    let passed = (i - MASTER_LUFS).abs() <= 0.2 && tp <= MASTER_TP + 0.3;

    Ok(Check {
        name: "loudness",
        passed,
        detail: format!("{i:.2} LUFS, {tp:.2} dBTP"),
    })
}

// ------------------------------------------------------------------ 2. a/v sync

fn check_sync(input: &str, out_path: &Path, temp_dir: &Path, plan: &Plan) -> Result<Check> {
    let n = 7;
    let mut scores = Vec::new();

    for k in 1..=n {
        let out_t = plan.out_duration_s * k as f64 / (n + 1) as f64;
        let src_t = match map_to_source(plan, out_t) {
            Some(t) => t,
            None => continue,
        };

        let a = temp_dir.join(format!("sync_out_{k}.png"));
        let b = temp_dir.join(format!("sync_src_{k}.png"));

        grab_frame(out_path.to_str().unwrap(), out_t, &a)?;
        grab_frame(input, src_t, &b)?;

        scores.push(ssim(&a, &b)?);
    }

    let worst = scores.iter().cloned().fold(1.0f64, f64::min);

    Ok(Check {
        name: "a/v sync",
        passed: worst >= 0.90,
        detail: format!("worst {worst:.3} over {} checkpoints", scores.len()),
    })
}

fn grab_frame(input: &str, t: f64, out_path: &Path) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args([
            "-y",
            "-ss",
            &format!("{t:.6}"),
            "-i",
            input,
            "-frames:v",
            "1",
            "-q:v",
            "2",
        ])
        .arg(out_path)
        .output()
        .context("could not run ffmpeg")?;

    if !out.status.success() {
        bail!(
            "frame grab failed at {t:.2}s: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn ssim(a: &Path, b: &Path) -> Result<f64> {
    let out = Command::new("ffmpeg")
        .args(["-i"])
        .arg(a)
        .args(["-i"])
        .arg(b)
        .args(["-lavfi", "ssim", "-f", "null", "-"])
        .output()
        .context("could not run ffmpeg")?;

    let stderr = String::from_utf8_lossy(&out.stderr);
    let all = stderr
        .split("All:")
        .nth(1)
        .context("no SSIM in ffmpeg output")?;
    let value = all.split_whitespace().next().context("bad SSIM output")?;
    value.parse().context("could not parse SSIM")
}

fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let reader =
        WavReader::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let rate = reader.spec().sample_rate;
    let samples: Vec<f32> = reader
        .into_samples::<f32>()
        .collect::<Result<Vec<f32>, _>>()?;
    Ok((samples, rate))
}

fn extract_verify_audio(out_path: &Path, wav_path: &Path) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(out_path)
        .args(["-vn", "-ac", "1", "-ar", "48000", "-c:a", "pcm_f32le"])
        .arg(wav_path)
        .output()
        .context("could not run ffmpeg")?;

    if !out.status.success() {
        bail!(
            "verify audio extraction failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Sample-to-sample jump at each splice, against the file's own p99.9.
fn click_stats(samples: &[f32], sample_rate: u32, plan: &Plan) -> (usize, f32, f32) {
    let mut diffs: Vec<f32> = samples.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p999 = diffs[(diffs.len() as f64 * 0.999) as usize];

    let mut worst = 0.0f32;
    let mut failures = 0;

    for seg in plan.segments.iter().skip(1) {
        let idx = (seg.out_start * sample_rate as f64) as usize;
        if idx == 0 || idx >= samples.len() {
            continue;
        }
        let jump = (samples[idx] - samples[idx - 1]).abs();
        worst = worst.max(jump);
        if jump > p999 {
            failures += 1;
        }
    }

    (failures, worst, p999)
}

/// `pre` is the same measurement on the pre-master concat, before the limiter
/// touches the signal.
///
/// The limiter smooths transients, which makes this check easier to pass —
/// measured 0.0443 -> 0.0334 on the demo. A check that got easier because we
/// changed the signal is not the same as a check that passed on merit, so the
/// pre-limiter number is reported alongside rather than quietly banked.
fn check_clicks(
    samples: &[f32],
    sample_rate: u32,
    plan: &Plan,
    pre: Option<(&[f32], u32)>,
) -> Result<Check> {
    let (failures, worst, p999) = click_stats(samples, sample_rate, plan);
    let n = plan.segments.len() - 1;

    let detail = match pre {
        Some((ps, pr)) => {
            let (_, pre_worst, _) = click_stats(ps, pr, plan);
            format!(
                "{failures} / {n} (worst {worst:.4} vs p99.9 {p999:.4}; pre-limiter worst {pre_worst:.4})"
            )
        }
        None => format!("{failures} / {n} (worst {worst:.4} vs p99.9 {p999:.4})"),
    };

    Ok(Check {
        name: "splice clicks",
        passed: failures == 0,
        detail,
    })
}

// ---------------------------------------------------------- duration vs plan

fn check_duration(out_path: &Path, plan: &Plan) -> Result<Check> {
    let json = ffprobe_json(out_path.to_str().context("non-UTF-8 path")?)?;
    let probe: Probe =
        serde_json::from_str(&json).context("could not parse ffprobe output for out.mp4")?;
    let measured: f64 = probe
        .format
        .duration
        .parse()
        .with_context(|| format!("bad out.mp4 duration {:?}", probe.format.duration))?;

    let planned = plan.out_duration_s;
    let diff = measured - planned;

    Ok(Check {
        name: "duration",
        // sped segments round to whole frames, so the file runs a touch long.
        // 0.5 s of slack per BUILD_STEPS M7; larger means the plan and the file
        // genuinely disagree.
        passed: diff.abs() <= 0.5,
        detail: format!("{measured:.2}s vs plan {planned:.2}s ({diff:+.2}s)"),
    })
}

pub fn verify(input: &str, out_path: &Path, temp_dir: &Path, plan: &Plan) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    // duration vs plan
    checks.push(check_duration(out_path, plan)?);

    // 5. faststart
    checks.push(check_faststart(out_path)?);

    // 4. loudness
    checks.push(check_loudness(out_path)?);

    // extract the delivered audio once — 1 and 2 both read it
    let verify_wav = temp_dir.join("verify.wav");
    extract_verify_audio(out_path, &verify_wav)?;
    let (samples, rate) = read_wav(&verify_wav)?;

    // 1. splice clicks — also measured on the pre-master concat, so the
    // limiter's smoothing of transients is visible rather than banked
    let concat_path = temp_dir.join("concat.mkv");
    let pre = if concat_path.exists() {
        let pre_wav = temp_dir.join("verify_pre.wav");
        extract_verify_audio(&concat_path, &pre_wav)?;
        Some(read_wav(&pre_wav)?)
    } else {
        None
    };
    checks.push(check_clicks(
        &samples,
        rate,
        plan,
        pre.as_ref().map(|(s, r)| (s.as_slice(), *r)),
    )?);

    // 2. a/v sync
    checks.push(check_sync(input, out_path, temp_dir, plan)?);

    Ok(checks)
}
