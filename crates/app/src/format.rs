//! Small pure formatting helpers used across the tabs. Kept free of egui so they
//! can be unit-tested directly.

use std::time::Duration;

/// Render a logical-core list as a compact comma-separated string.
///
/// `[0, 2, 4]` -> `"0, 2, 4"`; empty -> `"(none)"`.
pub fn cores(list: &[usize]) -> String {
    if list.is_empty() {
        return "(none)".to_string();
    }
    list.iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format an L3 cache size in bytes as whole mebibytes, e.g. `"96 MB"`.
///
/// Rounds to the nearest MB; sizes under 1 MB fall back to kibibytes.
pub fn cache_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        let mb = (bytes as f64 / MB as f64).round() as u64;
        format!("{mb} MB")
    } else {
        format!("{} KB", (bytes as f64 / 1024.0).round() as u64)
    }
}

/// Format byte capacities with binary units, keeping one decimal where useful.
pub fn bytes(bytes: u64) -> String {
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB)
    } else if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

/// Format an FPS value with one decimal place.
pub fn fps(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.1}")
    } else {
        "-".to_string()
    }
}

/// Format an elapsed duration compactly, e.g. `"0s"`, `"45s"`, `"2m 05s"`.
pub fn duration(d: Duration) -> String {
    let total = d.as_secs();
    let mins = total / 60;
    let secs = total % 60;
    if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Format a duration in seconds (session length) as `"1m 05s"` / `"45s"`.
pub fn duration_secs(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "-".to_string();
    }
    duration(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cores_basic() {
        assert_eq!(cores(&[0, 2, 4, 6, 8, 10]), "0, 2, 4, 6, 8, 10");
        assert_eq!(cores(&[3]), "3");
        assert_eq!(cores(&[]), "(none)");
    }

    #[test]
    fn cache_size_mb_and_kb() {
        assert_eq!(cache_size(96 * 1024 * 1024), "96 MB");
        assert_eq!(cache_size(32 * 1024 * 1024), "32 MB");
        assert_eq!(cache_size(512 * 1024), "512 KB");
    }

    #[test]
    fn bytes_uses_binary_capacity() {
        assert_eq!(bytes(2 * 1024 * 1024 * 1024 * 1024), "2.00 TiB");
        assert_eq!(bytes(16 * 1024 * 1024 * 1024), "16.0 GiB");
        assert_eq!(bytes(512 * 1024 * 1024), "512 MiB");
    }

    #[test]
    fn fps_one_decimal() {
        assert_eq!(fps(142.34), "142.3");
        assert_eq!(fps(60.0), "60.0");
        assert_eq!(fps(f64::INFINITY), "-");
    }

    #[test]
    fn duration_formats() {
        assert_eq!(duration(Duration::from_secs(5)), "5s");
        assert_eq!(duration(Duration::from_secs(45)), "45s");
        assert_eq!(duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn duration_secs_formats() {
        assert_eq!(duration_secs(65.0), "1m 05s");
        assert_eq!(duration_secs(9.0), "9s");
        assert_eq!(duration_secs(-1.0), "-");
    }
}
