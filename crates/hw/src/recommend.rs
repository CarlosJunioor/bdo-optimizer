//! BDO CPU-affinity recommendation engine.
//!
//! Encodes the affinity table from ACanadianDude's BDO performance guide,
//! matched by substrings of the detected CPU model string. For dual-CCD X3D
//! parts the static table is cross-checked against live L3 topology so the mask
//! always lands on the actual V-Cache CCD.

use crate::cpu::{vcache_ccd, CpuInfo};
use crate::mask::{cores_to_mask, mask_to_cores, normalize_hex};

/// A recommended affinity configuration for BDO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// Recommended affinity mask in hex, or `None` when no change is advised.
    pub mask_hex: Option<String>,
    /// Enabled logical core ids implied by `mask_hex`.
    pub cores: Vec<usize>,
    /// Alternate masks worth A/B testing (e.g. `["554"]` for the 7900X3D).
    pub alternates: Vec<String>,
    /// Human-readable explanation of the recommendation.
    pub explanation: String,
    /// Whether live L3 topology confirmed the static table. `None` when no
    /// topology cross-check applies (or topology was unavailable).
    pub topology_confirmed: Option<bool>,
}

impl Recommendation {
    fn from_mask(mask: &str, explanation: &str, alternates: &[&str]) -> Self {
        let cores = mask_to_cores(mask).unwrap_or_default();
        Recommendation {
            mask_hex: Some(mask.to_uppercase()),
            cores,
            alternates: alternates.iter().map(|s| s.to_uppercase()).collect(),
            explanation: explanation.to_string(),
            topology_confirmed: None,
        }
    }

    fn no_change(explanation: &str) -> Self {
        Recommendation {
            mask_hex: None,
            cores: Vec::new(),
            alternates: Vec::new(),
            explanation: explanation.to_string(),
            topology_confirmed: None,
        }
    }

    fn unknown() -> Self {
        Recommendation {
            mask_hex: None,
            cores: Vec::new(),
            alternates: Vec::new(),
            explanation: "No known profile for this CPU — benchmark masks manually.".to_string(),
            topology_confirmed: None,
        }
    }
}

/// Recommend a BDO affinity mask for the detected CPU.
///
/// For dual-CCD X3D chips, if L3 topology is available the mask is derived from
/// the actual V-Cache CCD's physical cores (SMT siblings excluded), and
/// [`Recommendation::topology_confirmed`] reports whether that matched the
/// static table.
pub fn recommend(cpu: &CpuInfo) -> Recommendation {
    let model = cpu.model.to_uppercase();

    let (mut rec, dual_ccd_x3d) = if is_amd(&model) {
        match_amd(&model, cpu)
    } else if is_intel(&model) {
        match_intel(cpu)
    } else {
        (Recommendation::unknown(), false)
    };

    if dual_ccd_x3d {
        apply_x3d_topology(&mut rec, cpu);
    }

    rec
}

fn is_amd(model: &str) -> bool {
    model.contains("AMD") || model.contains("RYZEN")
}

fn is_intel(model: &str) -> bool {
    model.contains("INTEL")
        || model.contains("CORE(TM)")
        || model.contains("I3-")
        || model.contains("I5-")
        || model.contains("I7-")
        || model.contains("I9-")
}

/// Cross-check / override the static X3D mask against live L3 topology.
fn apply_x3d_topology(rec: &mut Recommendation, cpu: &CpuInfo) {
    if cpu.l3_domains.is_empty() {
        rec.explanation
            .push_str(" L3 topology unavailable — using the guide's static table; verify the V-Cache CCD manually.");
        // Leave topology_confirmed = None: not checked.
        return;
    }

    match vcache_ccd(&cpu.l3_domains) {
        Some(ccd) => {
            // With SMT on, physical cores are the even logical ids (the sibling
            // is the odd neighbour), so the even-filtered list is exactly the
            // CCD's physical core count (6 on a 12-core X3D, 8 on a 16-core).
            //
            // With SMT disabled in BIOS every logical id already *is* a physical
            // core, and filtering evens would pin BDO to half the V-Cache CCD —
            // so take the CCD's cores as-is.
            let smt = cpu.physical_cores > 0 && cpu.logical_cores == cpu.physical_cores * 2;
            let mut cores: Vec<usize> = if smt {
                ccd.logical_cores
                    .iter()
                    .copied()
                    .filter(|c| c % 2 == 0)
                    .collect()
            } else {
                ccd.logical_cores.clone()
            };
            cores.sort_unstable();

            if let Ok(mask) = cores_to_mask(&cores) {
                let confirmed = rec
                    .mask_hex
                    .as_deref()
                    .map(|table| normalize_hex(table) == normalize_hex(&mask))
                    .unwrap_or(false);
                rec.topology_confirmed = Some(confirmed);
                if confirmed {
                    rec.explanation.push_str(
                        " L3 topology confirms the V-Cache CCD matches the guide's mask.",
                    );
                } else {
                    rec.explanation.push_str(
                        " L3 topology located the V-Cache CCD; mask derived from the actual CCD cores (differs from the static table).",
                    );
                    rec.cores = cores;
                    rec.mask_hex = Some(mask.to_uppercase());
                }
            }
        }
        None => {
            rec.topology_confirmed = Some(false);
            rec.explanation.push_str(
                " L3 topology did not reveal a distinct V-Cache CCD — falling back to the guide's static table.",
            );
        }
    }
}

/// Match an AMD Ryzen model string. Returns `(recommendation, is_dual_ccd_x3d)`.
fn match_amd(m: &str, _cpu: &CpuInfo) -> (Recommendation, bool) {
    // --- Dual-CCD X3D parts: must land on the V-Cache CCD (topology cross-check) ---
    // NOTE: the X3D arms must stay above their non-X3D counterparts — "9950X3D"
    // also contains "9950X", so reordering these silently mis-recommends.
    if m.contains("7950X3D") || m.contains("9950X3D") {
        return (
            Recommendation::from_mask(
                "5555",
                "Ryzen 9 16-core dual-CCD X3D: pin BDO to the 8-core V-Cache CCD, one thread per physical core.",
                &[],
            ),
            true,
        );
    }
    if m.contains("7900X3D") || m.contains("9900X3D") {
        return (
            Recommendation::from_mask(
                "555",
                "Ryzen 9 12-core dual-CCD X3D: pin BDO to the 6-core V-Cache CCD, one thread per physical core.",
                &["554"],
            ),
            true,
        );
    }

    // --- Single-CCX 8-core parts (incl. single-CCD X3D) ---
    if m.contains("5800X3D")
        || m.contains("7800X3D")
        || m.contains("9800X3D")
        || m.contains("5800X")
        || m.contains("7800X")
        || m.contains("9700X")
    {
        return (
            Recommendation::from_mask(
                "5554",
                "8-core single-CCX Ryzen: one thread per physical core, core 0 disabled per the guide.",
                &[],
            ),
            false,
        );
    }

    // --- Ryzen 9 non-X3D, 16-core / 2 CCD ---
    if m.contains("3950X") || m.contains("5950X") || m.contains("7950X") || m.contains("9950X") {
        return (
            Recommendation::from_mask(
                "5550000",
                "Ryzen 9 16-core (2 CCD): pin BDO to one CCD, one thread per physical core.",
                &["5550"],
            ),
            false,
        );
    }

    // --- Ryzen 9 non-X3D, 12-core / 2 CCD ---
    if m.contains("3900") || m.contains("5900X") || m.contains("7900X") || m.contains("9900X") {
        return (
            Recommendation::from_mask(
                "555000",
                "Ryzen 9 12-core (2 CCD): pin BDO to one CCD, one thread per physical core.",
                &["555"],
            ),
            false,
        );
    }

    // --- Ryzen 7 8-core, Zen2 ---
    if m.contains("3700X") || m.contains("3800X") {
        return (
            Recommendation::from_mask(
                "5550",
                "Ryzen 7 3700X/3800X (8-core): one thread per physical core.",
                &[],
            ),
            false,
        );
    }

    // --- Ryzen 7 8-core, Zen/Zen+ (2 CCX) ---
    if m.contains("1700") || m.contains("1800X") || m.contains("2700") {
        return (
            Recommendation::from_mask(
                "5500",
                "Ryzen 7 (Zen/Zen+) 8-core: guide mask targeting the second CCX.",
                &[],
            ),
            false,
        );
    }

    // --- Ryzen 5 6-core, modern (Zen2+) ---
    if m.contains("3600") || m.contains("5600") || m.contains("7600") || m.contains("9600") {
        return (
            Recommendation::from_mask("555", "Ryzen 5 6-core: one thread per physical core.", &[]),
            false,
        );
    }

    // --- Ryzen 5 3500/3500X: 6 cores / 6 threads, no SMT ---
    if m.contains("3500") {
        return (
            Recommendation::no_change(
                "Ryzen 5 3500/3500X has 6 cores / 6 threads — no SMT to disable, no change needed.",
            ),
            false,
        );
    }

    // --- Ryzen 5 6-core, Zen/Zen+ (2 CCX) ---
    if m.contains("1600") || m.contains("2600") {
        return (
            Recommendation::from_mask(
                "540",
                "Ryzen 5 (Zen/Zen+) 6-core (2 CCX): guide mask spanning both CCX.",
                &[],
            ),
            false,
        );
    }

    // --- Ryzen 5 4-core / 8-thread (APUs) ---
    if m.contains("1400")
        || m.contains("1500X")
        || m.contains("2400G")
        || m.contains("2500X")
        || m.contains("3400G")
    {
        return (
            Recommendation::from_mask("50", "Ryzen 5 4-core / 8-thread: guide mask.", &[]),
            false,
        );
    }

    // --- Ryzen 3 4-core / 4-thread ---
    if m.contains("1200") || m.contains("1300X") || m.contains("2200G") || m.contains("3200G") {
        return (
            Recommendation::from_mask("C", "Ryzen 3 4-core / 4-thread: guide mask.", &[]),
            false,
        );
    }

    (Recommendation::unknown(), false)
}

/// Match an Intel Core model using core counts and SMT layout.
fn match_intel(cpu: &CpuInfo) -> (Recommendation, bool) {
    let p = cpu.physical_cores;
    let l = cpu.logical_cores;

    // Hybrid (P + E cores, 12th gen+): logical is between physical and 2x
    // physical, because only P-cores have SMT.
    let hybrid = p > 0 && l > p && l < p * 2;
    if hybrid {
        return (
            Recommendation {
                mask_hex: None,
                cores: Vec::new(),
                alternates: Vec::new(),
                explanation: "12th-gen+ hybrid CPU (P + E cores). Recommendation: disable E-cores and run BDO on the P-cores only. Per-core P/E topology was not available, so no exact mask was generated — identify the P-core logical ids and benchmark.".to_string(),
                topology_confirmed: None,
            },
            false,
        );
    }

    let ht = p > 0 && l == p * 2;
    if ht {
        let rec = match p {
            4 => Recommendation::from_mask(
                "AA",
                "Intel 4-core + HyperThreading: one thread per physical core.",
                &[],
            ),
            6 => Recommendation::from_mask(
                "AAA",
                "Intel 6-core + HyperThreading: one thread per physical core.",
                &[],
            ),
            _ if p >= 8 => Recommendation::from_mask(
                "AAA0",
                "Intel 8+ core + HyperThreading: guide mask (upper physical cores, siblings disabled).",
                &[],
            ),
            _ => Recommendation::unknown(),
        };
        return (rec, false);
    }

    // No HyperThreading.
    let rec = match p {
        8 => Recommendation::from_mask(
            "FC",
            "Intel 8-core without HyperThreading (e.g. i7-9700K): disable cores 0-1, run on cores 2-7.",
            &[],
        ),
        6 => Recommendation::no_change(
            "Intel 6-core without HyperThreading — no SMT to disable, no change needed.",
        ),
        _ => Recommendation::unknown(),
    };
    (rec, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::L3Domain;

    fn cpu(model: &str, physical: usize, logical: usize) -> CpuInfo {
        CpuInfo {
            model: model.to_string(),
            physical_cores: physical,
            logical_cores: logical,
            l3_domains: Vec::new(),
            caches: Vec::new(),
        }
    }

    fn dom(size_mb: u64, cores: Vec<usize>) -> L3Domain {
        L3Domain {
            size_bytes: size_mb * 1024 * 1024,
            logical_cores: cores,
        }
    }

    #[test]
    fn matches_7950x3d_no_topology() {
        let rec = recommend(&cpu("AMD Ryzen 9 7950X3D 16-Core Processor", 16, 32));
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10, 12, 14]);
        // No l3 domains -> not checked.
        assert_eq!(rec.topology_confirmed, None);
    }

    #[test]
    fn matches_zen5_x3d_and_non_x3d_parts() {
        // Zen 5 must resolve, not fall through to "no known profile".
        let z = |model: &str, p: usize, l: usize| {
            recommend(&cpu(model, p, l)).mask_hex.unwrap_or_default()
        };
        assert_eq!(z("AMD Ryzen 7 9800X3D 8-Core Processor", 8, 16), "5554");
        assert_eq!(z("AMD Ryzen 9 9950X3D 16-Core Processor", 16, 32), "5555");
        assert_eq!(z("AMD Ryzen 9 9900X3D 12-Core Processor", 12, 24), "555");
        assert_eq!(z("AMD Ryzen 7 9700X 8-Core Processor", 8, 16), "5554");
        assert_eq!(z("AMD Ryzen 5 9600X 6-Core Processor", 6, 12), "555");
        // The X3D arms must win over their non-X3D substring counterparts.
        assert_eq!(z("AMD Ryzen 9 9950X 16-Core Processor", 16, 32), "5550000");
        assert_eq!(z("AMD Ryzen 9 9900X 12-Core Processor", 12, 24), "555000");
    }

    #[test]
    fn x3d_topology_keeps_all_cores_when_smt_is_disabled() {
        // SMT off: 16 physical == 16 logical, V-Cache CCD holds cores 0..7.
        // Filtering evens here would pin BDO to half the CCD.
        let mut c = cpu("AMD Ryzen 9 7950X3D 16-Core Processor", 16, 16);
        c.l3_domains = vec![dom(96, (0..8).collect()), dom(32, (8..16).collect())];
        let rec = recommend(&c);
        assert_eq!(rec.cores, vec![0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rec.mask_hex.as_deref(), Some("FF"));
    }

    #[test]
    fn matches_7900x3d_has_554_alternate() {
        let rec = recommend(&cpu("AMD Ryzen 9 7900X3D 12-Core Processor", 12, 24));
        assert_eq!(rec.mask_hex.as_deref(), Some("555"));
        assert!(rec.alternates.contains(&"554".to_string()));
    }

    #[test]
    fn x3d_topology_confirms_table() {
        // V-Cache CCD is the first CCD with even cores 0..14 -> mask 5555.
        let mut c = cpu("AMD Ryzen 9 7950X3D 16-Core Processor", 16, 32);
        c.l3_domains = vec![dom(96, (0..16).collect()), dom(32, (16..32).collect())];
        let rec = recommend(&c);
        assert_eq!(rec.topology_confirmed, Some(true));
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
    }

    #[test]
    fn x3d_topology_overrides_table_when_vcache_is_second_ccd() {
        // V-Cache CCD is the SECOND die (cores 16..32). Table says 5555 (CCD0)
        // but topology says the mask must target cores 16,18,20,22,24,26.
        let mut c = cpu("AMD Ryzen 9 7950X3D 16-Core Processor", 16, 32);
        c.l3_domains = vec![dom(32, (0..16).collect()), dom(96, (16..32).collect())];
        let rec = recommend(&c);
        assert_eq!(rec.topology_confirmed, Some(false));
        // All 8 physical cores of the V-Cache CCD (even logical ids).
        assert_eq!(rec.cores, vec![16, 18, 20, 22, 24, 26, 28, 30]);
        assert_eq!(rec.mask_hex.as_deref(), Some("55550000"));
    }

    #[test]
    fn x3d_7900x3d_topology_confirms_6core_ccd() {
        // 7900X3D V-Cache CCD = 6 physical cores (12 logical), cores 0..12.
        let mut c = cpu("AMD Ryzen 9 7900X3D 12-Core Processor", 12, 24);
        c.l3_domains = vec![dom(96, (0..12).collect()), dom(32, (12..24).collect())];
        let rec = recommend(&c);
        assert_eq!(rec.topology_confirmed, Some(true));
        assert_eq!(rec.mask_hex.as_deref(), Some("555"));
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10]);
    }

    #[test]
    fn matches_5800x3d() {
        let rec = recommend(&cpu("AMD Ryzen 7 5800X3D 8-Core Processor", 8, 16));
        assert_eq!(rec.mask_hex.as_deref(), Some("5554"));
        // Single-CCD X3D: no dual-CCD topology cross-check.
        assert_eq!(rec.topology_confirmed, None);
    }

    #[test]
    fn matches_3600() {
        let rec = recommend(&cpu("AMD Ryzen 5 3600 6-Core Processor", 6, 12));
        assert_eq!(rec.mask_hex.as_deref(), Some("555"));
    }

    #[test]
    fn matches_1600x() {
        let rec = recommend(&cpu("AMD Ryzen 5 1600X Six-Core Processor", 6, 12));
        assert_eq!(rec.mask_hex.as_deref(), Some("540"));
    }

    #[test]
    fn matches_5950x_not_x3d() {
        let rec = recommend(&cpu("AMD Ryzen 9 5950X 16-Core Processor", 16, 32));
        assert_eq!(rec.mask_hex.as_deref(), Some("5550000"));
        assert!(rec.alternates.contains(&"5550".to_string()));
        assert_eq!(rec.topology_confirmed, None);
    }

    #[test]
    fn matches_3500_no_change() {
        let rec = recommend(&cpu("AMD Ryzen 5 3500X 6-Core Processor", 6, 6));
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("no SMT"));
    }

    #[test]
    fn matches_i7_9700k() {
        let rec = recommend(&cpu("Intel(R) Core(TM) i7-9700K CPU @ 3.60GHz", 8, 8));
        assert_eq!(rec.mask_hex.as_deref(), Some("FC"));
        assert_eq!(rec.cores, vec![2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn matches_intel_6core_ht() {
        let rec = recommend(&cpu("Intel(R) Core(TM) i5-8600... ", 6, 12));
        assert_eq!(rec.mask_hex.as_deref(), Some("AAA"));
    }

    #[test]
    fn matches_intel_hybrid_no_mask() {
        // 13900K: 8 P-cores (HT) + 16 E-cores = 24 physical, 32 logical.
        let rec = recommend(&cpu("Intel(R) Core(TM) i9-13900K", 24, 32));
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("E-core"));
    }

    #[test]
    fn unknown_cpu() {
        let rec = recommend(&cpu("Totally Unknown Vendor CPU 9000", 8, 16));
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("benchmark"));
    }
}
