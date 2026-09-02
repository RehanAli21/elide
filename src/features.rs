use crate::constants::ENERGY_S;

pub fn energy_db(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    let frame = (sample_rate as f64 * ENERGY_S) as usize; // 160 samples
    let n = samples.len() / frame;
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let chunk = &samples[i * frame..(i + 1) * frame];
        let sum: f64 = chunk.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (sum / frame as f64).sqrt();
        out.push(20.0 * rms.max(1e-10).log10());
    }

    out
}

pub fn smooth(energy: &[f64], window_s: f64) -> Vec<f64> {
    let mut k = (window_s / ENERGY_S) as usize; // 0.03 / 0.01 = 3
    if k.is_multiple_of(2) {
        k += 1; // must be odd, so there's a centre
    }
    let half = k / 2; // 1

    let mut out = Vec::with_capacity(energy.len());

    for i in 0..energy.len() {
        let lo = i.saturating_sub(half); // one before, or 0 at the start
        let hi = (i + half + 1).min(energy.len()); // one after, or the end
        let sum: f64 = energy[lo..hi].iter().sum();
        out.push(sum / (hi - lo) as f64); // average
    }

    out
}
