use crate::constants::{ENERGY_S, ENERGY_WIN_S};

/// Energy per frame, in dB.
///
/// Frames advance by ENERGY_S (10 ms) but each covers ENERGY_WIN_S (32 ms), so
/// consecutive frames share 22 ms of audio. The overlap matters: with
/// non-overlapping 10 ms blocks the array is far spikier, and a spiky array
/// trips the edge guard on transients that are not speech.
///
/// Each frame is tapered by a Hann window before the RMS. The taper is not
/// cosmetic: it means a loud onset at the frame's edge contributes far less
/// than one at its centre, so a single onset does not light up three
/// consecutive frames equally. Hann RMS is 0.6124x rectangular, so windowed
/// frames read about 4.26 dB lower — the gate is fixed, so the energy scale
/// has to match the reference or the gate means something different.
///
/// RMS is taken in the linear domain and converted once. Averaging dB values
/// instead of amplitudes gives a different, lower number.
pub fn energy_db(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    let hop = (sample_rate as f64 * ENERGY_S) as usize; // 160 samples
    let nfft = (sample_rate as f64 * ENERGY_WIN_S) as usize; // 512 samples
    let n = samples.len() / hop; // keeps len == grid_len * 2

    // symmetric Hann, matching numpy's np.hanning (denominator nfft - 1)
    let win: Vec<f64> = (0..nfft)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (nfft - 1) as f64).cos())
        .collect();

    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let lo = i * hop;
        let hi = (lo + nfft).min(samples.len());
        if hi <= lo {
            out.push(20.0 * 1e-10f64.log10());
            continue;
        }
        let chunk = &samples[lo..hi];
        let sum: f64 = chunk
            .iter()
            .zip(&win)
            .map(|(&s, &w)| {
                let v = s as f64 * w;
                v * v
            })
            .sum();
        let rms = (sum / chunk.len() as f64).sqrt();
        out.push(20.0 * rms.max(1e-10).log10());
    }

    out
}

/// Centred boxcar mean over exactly `k` energy frames.
pub fn boxcar(energy: &[f64], k: usize) -> Vec<f64> {
    let k = k.max(1);
    let half = k / 2;
    let mut out = Vec::with_capacity(energy.len());

    for i in 0..energy.len() {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(energy.len());
        let sum: f64 = energy[lo..hi].iter().sum();
        out.push(sum / (hi - lo) as f64);
    }

    out
}

/// Smoothing for the disfluency splice test and the quiet-run guard: 30 ms,
/// window forced odd so there is a true centre.
///
/// The edge guard uses a different window (100 ms) — see `boxcar` and
/// `EDGE_SMOOTH_S`. Two jobs, two windows: a 10 ms click 20 dB over gate lifts a
/// 100 ms mean by only ~2 dB, so it will not trip the edge guard, while
/// sustained speech crosses easily.
pub fn smooth(energy: &[f64], window_s: f64) -> Vec<f64> {
    let mut k = (window_s / ENERGY_S) as usize; // 0.03 / 0.01 = 3
    if k.is_multiple_of(2) {
        k += 1; // must be odd, so there's a centre
    }
    boxcar(energy, k)
}
