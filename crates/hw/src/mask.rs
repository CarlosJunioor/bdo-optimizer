//! Affinity-mask <-> logical-core-list conversion helpers.
//!
//! Windows affinity masks are hexadecimal bitmaps where bit `n` set means
//! logical processor `n` is enabled. A single processor group holds at most 64
//! logical processors, so a `u64` is sufficient for every mask in the guide.

use std::error::Error;
use std::fmt;

/// Error returned when parsing or building an affinity mask fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaskError {
    /// The supplied hex string was empty (after trimming an optional `0x`).
    Empty,
    /// The supplied string was not valid hexadecimal, or exceeded 64 bits.
    InvalidHex(String),
    /// A core index was >= 64 and cannot be represented in a single-group mask.
    CoreTooLarge(usize),
}

impl fmt::Display for MaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MaskError::Empty => write!(f, "empty affinity mask"),
            MaskError::InvalidHex(s) => write!(f, "invalid hexadecimal affinity mask: {s:?}"),
            MaskError::CoreTooLarge(c) => {
                write!(f, "core index {c} exceeds single-group mask width (64)")
            }
        }
    }
}

impl Error for MaskError {}

/// Parse a hexadecimal affinity mask into the sorted list of enabled logical
/// core indices.
///
/// Accepts an optional `0x` / `0X` prefix and surrounding whitespace.
///
/// ```
/// # use bdo_hw::mask_to_cores;
/// assert_eq!(mask_to_cores("555").unwrap(), vec![0, 2, 4, 6, 8, 10]);
/// assert_eq!(mask_to_cores("C").unwrap(), vec![2, 3]);
/// ```
pub fn mask_to_cores(hex: &str) -> Result<Vec<usize>, MaskError> {
    let trimmed = hex.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if body.is_empty() {
        return Err(MaskError::Empty);
    }
    let value =
        u64::from_str_radix(body, 16).map_err(|_| MaskError::InvalidHex(hex.to_string()))?;
    Ok((0..64u32)
        .filter(|i| value & (1u64 << i) != 0)
        .map(|i| i as usize)
        .collect())
}

/// Build a lowercase hexadecimal affinity mask from a list of logical core
/// indices.
///
/// ```
/// # use bdo_hw::cores_to_mask;
/// assert_eq!(cores_to_mask(&[0, 2, 4, 6, 8, 10]).unwrap(), "555");
/// ```
pub fn cores_to_mask(cores: &[usize]) -> Result<String, MaskError> {
    let mut value: u64 = 0;
    for &c in cores {
        if c >= 64 {
            return Err(MaskError::CoreTooLarge(c));
        }
        value |= 1u64 << c;
    }
    Ok(format!("{value:x}"))
}

/// Normalize a hex mask for comparison: lowercase, no `0x` prefix, no leading
/// zeros.
pub(crate) fn normalize_hex(s: &str) -> String {
    let t = s.trim();
    let body = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let stripped = body.trim_start_matches('0');
    if stripped.is_empty() {
        "0".to_string()
    } else {
        stripped.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_to_cores_basic() {
        assert_eq!(mask_to_cores("C").unwrap(), vec![2, 3]);
        assert_eq!(mask_to_cores("50").unwrap(), vec![4, 6]);
        assert_eq!(mask_to_cores("540").unwrap(), vec![6, 8, 10]);
        assert_eq!(mask_to_cores("5500").unwrap(), vec![8, 10, 12, 14]);
        assert_eq!(mask_to_cores("555").unwrap(), vec![0, 2, 4, 6, 8, 10]);
        assert_eq!(mask_to_cores("5554").unwrap(), vec![2, 4, 6, 8, 10, 12, 14]);
        assert_eq!(
            mask_to_cores("5555").unwrap(),
            vec![0, 2, 4, 6, 8, 10, 12, 14]
        );
        assert_eq!(
            mask_to_cores("555000").unwrap(),
            vec![12, 14, 16, 18, 20, 22]
        );
        assert_eq!(
            mask_to_cores("5550000").unwrap(),
            vec![16, 18, 20, 22, 24, 26]
        );
    }

    #[test]
    fn mask_to_cores_intel() {
        assert_eq!(mask_to_cores("FC").unwrap(), vec![2, 3, 4, 5, 6, 7]);
        assert_eq!(mask_to_cores("AA").unwrap(), vec![1, 3, 5, 7]);
        assert_eq!(mask_to_cores("AAA").unwrap(), vec![1, 3, 5, 7, 9, 11]);
        assert_eq!(mask_to_cores("AAA0").unwrap(), vec![5, 7, 9, 11, 13, 15]);
    }

    #[test]
    fn mask_to_cores_prefix_and_whitespace() {
        assert_eq!(mask_to_cores("  0x555  ").unwrap(), vec![0, 2, 4, 6, 8, 10]);
        assert_eq!(mask_to_cores("0X555").unwrap(), vec![0, 2, 4, 6, 8, 10]);
    }

    #[test]
    fn mask_to_cores_errors() {
        assert_eq!(mask_to_cores(""), Err(MaskError::Empty));
        assert_eq!(mask_to_cores("0x"), Err(MaskError::Empty));
        assert!(matches!(
            mask_to_cores("xyz"),
            Err(MaskError::InvalidHex(_))
        ));
        // 17 hex digits overflows u64.
        assert!(matches!(
            mask_to_cores("1ffffffffffffffff"),
            Err(MaskError::InvalidHex(_))
        ));
    }

    #[test]
    fn cores_to_mask_basic() {
        assert_eq!(cores_to_mask(&[2, 3]).unwrap(), "c");
        assert_eq!(cores_to_mask(&[0, 2, 4, 6, 8, 10]).unwrap(), "555");
        assert_eq!(cores_to_mask(&[0, 2, 4]).unwrap(), "15");
        assert_eq!(cores_to_mask(&[]).unwrap(), "0");
    }

    #[test]
    fn cores_to_mask_too_large() {
        assert_eq!(cores_to_mask(&[64]), Err(MaskError::CoreTooLarge(64)));
    }

    #[test]
    fn round_trip_all_guide_masks() {
        for m in [
            "C", "50", "540", "5500", "555", "5550", "5554", "5555", "555000", "5550000", "AA",
            "AAA", "AAA0", "FC",
        ] {
            let cores = mask_to_cores(m).unwrap();
            let back = cores_to_mask(&cores).unwrap();
            assert_eq!(
                normalize_hex(m),
                normalize_hex(&back),
                "round trip failed for mask {m}"
            );
        }
    }

    #[test]
    fn normalize_hex_works() {
        assert_eq!(normalize_hex("0x0555"), "555");
        assert_eq!(normalize_hex("555"), "555");
        assert_eq!(normalize_hex("0X00C"), "c");
        assert_eq!(normalize_hex("0"), "0");
    }
}
