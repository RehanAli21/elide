pub fn fmt_time(t: f64) -> String {
    let mins = (t / 60.0).floor();
    let secs = t - mins * 60.0;
    format!("{mins:02.0}:{secs:04.1}")
}
