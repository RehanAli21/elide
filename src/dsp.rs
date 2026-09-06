//! Small DSP helpers for the acoustic test in M11.
//!
//! Two takes of a repeated phrase must actually *sound* alike before we cut
//! one, otherwise matching words from a mis-transcription would trigger a cut.
//! That needs a spectrum, so this carries a minimal radix-2 FFT rather than
//! pulling in an FFT crate.

use std::f64::consts::PI;

/// In-place iterative radix-2 FFT. `re`/`im` must be the same power-of-two length.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();

    // bit-reversal permutation
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        let half = len / 2;
        let mut i = 0;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..half {
                let (ur, ui) = (re[i + k], im[i + k]);
                let vr = re[i + k + half] * cr - im[i + k + half] * ci;
                let vi = re[i + k + half] * ci + im[i + k + half] * cr;
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + half] = ur - vr;
                im[i + k + half] = ui - vi;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
            i += len;
        }
        len <<= 1;
    }
}

const N: usize = 512;
const HOP: usize = 160;
const BANDS: usize = 24;

/// Log band energies over `[a, b)` seconds: one row per frame, BANDS columns.
/// Returns None if the span is too short to give at least 3 frames.
fn bandspec(samples: &[f32], sr: u32, a: f64, b: f64) -> Option<Vec<Vec<f64>>> {
    let i0 = (a * sr as f64).max(0.0) as usize;
    let i1 = ((b * sr as f64) as usize).min(samples.len());
    if i1 <= i0 || i1 - i0 < N {
        return None;
    }
    let seg = &samples[i0..i1];
    let frames = 1 + (seg.len() - N) / HOP;
    if frames < 3 {
        return None;
    }

    // Hann window, precomputed
    let win: Vec<f64> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / N as f64).cos())
        .collect();

    let bins = N / 2 + 1;
    let mut out = Vec::with_capacity(frames);

    for f in 0..frames {
        let mut re: Vec<f64> = (0..N)
            .map(|i| seg[f * HOP + i] as f64 * win[i])
            .collect();
        let mut im = vec![0.0f64; N];
        fft(&mut re, &mut im);

        // power spectrum, then sum into equal-width bands
        let mut row = vec![0.0f64; BANDS];
        for (band, slot) in row.iter_mut().enumerate() {
            let lo = band * bins / BANDS;
            let hi = ((band + 1) * bins / BANDS).max(lo + 1).min(bins);
            let mut sum = 0.0;
            for k in lo..hi {
                sum += re[k] * re[k] + im[k] * im[k];
            }
            *slot = (sum + 1e-12).ln();
        }
        out.push(row);
    }

    Some(out)
}

/// Resample `m` to exactly `l` rows by linear interpolation down each column.
fn resample(m: &[Vec<f64>], l: usize) -> Vec<Vec<f64>> {
    let n = m.len();
    (0..l)
        .map(|i| {
            let pos = if l > 1 {
                i as f64 * (n - 1) as f64 / (l - 1) as f64
            } else {
                0.0
            };
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            let t = pos - lo as f64;
            (0..BANDS)
                .map(|c| m[lo][c] * (1.0 - t) + m[hi][c] * t)
                .collect()
        })
        .collect()
}

/// Correlation between two takes, time-normalised to the same length.
/// 1.0 = identical spectra, 0.0 = unrelated. Returns 0.0 if either span is too
/// short to measure.
pub fn similarity(samples: &[f32], sr: u32, a1: f64, b1: f64, a2: f64, b2: f64) -> f64 {
    let (m1, m2) = match (bandspec(samples, sr, a1, b1), bandspec(samples, sr, a2, b2)) {
        (Some(x), Some(y)) => (x, y),
        _ => return 0.0,
    };
    let l = m1.len().min(m2.len());
    if l < 3 {
        return 0.0;
    }
    let r1 = resample(&m1, l);
    let r2 = resample(&m2, l);

    let z = |m: &Vec<Vec<f64>>| -> Vec<f64> {
        let flat: Vec<f64> = m.iter().flatten().copied().collect();
        let mean = flat.iter().sum::<f64>() / flat.len() as f64;
        let var = flat.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / flat.len() as f64;
        let sd = var.sqrt() + 1e-9;
        flat.iter().map(|v| (v - mean) / sd).collect()
    };

    let z1 = z(&r1);
    let z2 = z(&r2);
    z1.iter().zip(&z2).map(|(x, y)| x * y).sum::<f64>() / z1.len() as f64
}
