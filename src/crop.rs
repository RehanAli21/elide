use crate::constants::{ACTIVITY_MIN, ACTIVITY_RATIO, BUSY_DIFF, GRID_H, GRID_W, SAMPLE_FRAMES};
use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn sample_frames(input: &str, duration: f64) -> Result<Vec<Vec<u8>>> {
    let fps = SAMPLE_FRAMES as f64 / duration;
    let filter = format!("fps={fps},scale=160:90,format=gray");

    let out = Command::new("ffmpeg")
        .args([
            "-i", input, "-vf", &filter, "-an", "-f", "rawvideo", "-pix_fmt", "gray", "-",
        ])
        .output()
        .context("could not run ffmpeg. Is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed to sample frames: {}", stderr.trim());
    }

    let frames: Vec<Vec<u8>> = out
        .stdout
        .chunks_exact(GRID_W * GRID_H)
        .map(|c| c.to_vec())
        .collect();

    Ok(frames)
}

pub fn cell_activity(frames: &[Vec<u8>]) -> Vec<f32> {
    let cells = GRID_W * GRID_H;
    let mut sums = vec![0.0f32; cells];

    for pair in frames.windows(2) {
        for c in 0..cells {
            let diff = (pair[1][c] as i16 - pair[0][c] as i16).abs();
            sums[c] += diff as f32;
        }
    }

    let n = (frames.len() - 1) as f32;
    sums.iter().map(|s| s / n).collect()
}

pub fn content_mask(activity: &[f32]) -> Option<(Vec<bool>, f32)> {
    let max = activity.iter().fold(0.0f32, |m, &a| m.max(a));

    if max < ACTIVITY_MIN {
        return None;
    }

    let threshold = ACTIVITY_RATIO * max;
    let mask = activity.iter().map(|&a| a > threshold).collect();
    Some((mask, threshold))
}

pub fn blobs(mask: &[bool], w: usize, h: usize) -> Vec<Vec<usize>> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();

    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }

        let mut blob = Vec::new();
        let mut stack = vec![start];
        seen[start] = true;

        while let Some(c) = stack.pop() {
            blob.push(c);
            let (x, y) = (c % w, c / w);

            if x > 0 {
                push_if(&mut stack, &mut seen, mask, c - 1);
            }
            if x + 1 < w {
                push_if(&mut stack, &mut seen, mask, c + 1);
            }
            if y > 0 {
                push_if(&mut stack, &mut seen, mask, c - w);
            }
            if y + 1 < h {
                push_if(&mut stack, &mut seen, mask, c + w);
            }
        }

        out.push(blob);
    }

    out.sort_by_key(|b| std::cmp::Reverse(b.len()));
    out
}

fn push_if(stack: &mut Vec<usize>, seen: &mut [bool], mask: &[bool], c: usize) {
    if mask[c] && !seen[c] {
        seen[c] = true;
        stack.push(c);
    }
}

pub fn bounding_box(blob: &[usize], w: usize) -> (usize, usize, usize, usize) {
    let mut x0 = usize::MAX;
    let mut y0 = usize::MAX;
    let mut x1 = 0;
    let mut y1 = 0;

    for &c in blob {
        let (x, y) = (c % w, c / w);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }

    (x0, y0, x1, y1)
}

pub fn busy_fraction(blob: &[usize], frames: &[Vec<u8>]) -> f64 {
    let mut busy = 0;

    for pair in frames.windows(2) {
        let mut sum = 0.0f64;
        for &c in blob {
            sum += (pair[1][c] as i16 - pair[0][c] as i16).abs() as f64;
        }
        let mean = sum / blob.len() as f64;

        if mean > BUSY_DIFF {
            busy += 1;
        }
    }

    busy as f64 / (frames.len() - 1) as f64
}
