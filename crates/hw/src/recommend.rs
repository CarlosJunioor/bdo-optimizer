//! BDO CPU-affinity recommendation engine.
//!
//! Two layers. The static table encodes the affinity masks ACanadianDude's BDO
//! performance guide publishes for the CPUs it names, matched by substrings of
//! the detected model string; for dual-CCD X3D parts that table is
//! cross-checked against live L3 topology so the mask always lands on the
//! actual V-Cache CCD. Every CPU the table does not name — hybrid Intel,
//! Threadripper, new releases — falls through to [`derive_from_topology`],
//! which builds the mask from what the machine actually reports: efficiency
//! classes, L3 domains, and SMT siblings. The table is the guide's voice; the
//! derivation is what keeps the app useful on hardware the guide never saw.

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

    let table = if is_amd(&model) {
        match_amd(&model, cpu)
    } else if is_intel(&model) {
        match_intel(cpu)
    } else {
        None
    };

    match table {
        Some((mut rec, dual_ccd_x3d)) => {
            if dual_ccd_x3d {
                apply_x3d_topology(&mut rec, cpu);
            }
            rec
        }
        None => derive_from_topology(cpu).unwrap_or_else(Recommendation::unknown),
    }
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
            // One thread per physical core within the V-Cache CCD. Real SMT
            // sibling data is used when available; the fallback assumes evens
            // are physical with SMT on, and with SMT off in BIOS every id
            // already *is* a physical core (filtering evens then would pin BDO
            // to half the CCD).
            let cores = one_thread_per_core(cpu, Some(&ccd.logical_cores));

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

/// Match an AMD Ryzen model string against the guide's table.
/// Returns `Some((recommendation, is_dual_ccd_x3d))`, or `None` for models the
/// guide does not name (those derive from topology instead).
fn match_amd(m: &str, _cpu: &CpuInfo) -> Option<(Recommendation, bool)> {
    // --- Dual-CCD X3D parts: must land on the V-Cache CCD (topology cross-check) ---
    // NOTE: the X3D arms must stay above their non-X3D counterparts — "9950X3D"
    // also contains "9950X", so reordering these silently mis-recommends.
    if m.contains("7950X3D") || m.contains("9950X3D") {
        return Some((
            Recommendation::from_mask(
                "5555",
                "Ryzen 9 16-core dual-CCD X3D: pin BDO to the 8-core V-Cache CCD, one thread per physical core.",
                &[],
            ),
            true,
        ));
    }
    if m.contains("7900X3D") || m.contains("9900X3D") {
        return Some((
            Recommendation::from_mask(
                "555",
                "Ryzen 9 12-core dual-CCD X3D: pin BDO to the 6-core V-Cache CCD, one thread per physical core.",
                &["554"],
            ),
            true,
        ));
    }

    // --- Single-CCX 8-core parts (incl. single-CCD X3D) ---
    if m.contains("5800X3D")
        || m.contains("5700X3D")
        || m.contains("7800X3D")
        || m.contains("9800X3D")
        || m.contains("5800X")
        || m.contains("7800X")
        || m.contains("9700X")
    {
        return Some((
            Recommendation::from_mask(
                "5554",
                "8-core single-CCX Ryzen: one thread per physical core, core 0 disabled per the guide.",
                &[],
            ),
            false,
        ));
    }

    // --- Ryzen 9 non-X3D, 16-core / 2 CCD ---
    if m.contains("3950X") || m.contains("5950X") || m.contains("7950X") || m.contains("9950X") {
        return Some((
            Recommendation::from_mask(
                "5550000",
                "Ryzen 9 16-core (2 CCD): pin BDO to one CCD, one thread per physical core.",
                &["5550"],
            ),
            false,
        ));
    }

    // --- Ryzen 9 non-X3D, 12-core / 2 CCD ---
    if m.contains("3900") || m.contains("5900X") || m.contains("7900X") || m.contains("9900X") {
        return Some((
            Recommendation::from_mask(
                "555000",
                "Ryzen 9 12-core (2 CCD): pin BDO to one CCD, one thread per physical core.",
                &["555"],
            ),
            false,
        ));
    }

    // --- Ryzen 7 8-core, Zen2 ---
    if m.contains("3700X") || m.contains("3800X") {
        return Some((
            Recommendation::from_mask(
                "5550",
                "Ryzen 7 3700X/3800X (8-core): one thread per physical core.",
                &[],
            ),
            false,
        ));
    }

    // --- Ryzen 7 8-core, Zen/Zen+ (2 CCX) ---
    if m.contains("1700") || m.contains("1800X") || m.contains("2700") {
        return Some((
            Recommendation::from_mask(
                "5500",
                "Ryzen 7 (Zen/Zen+) 8-core: guide mask targeting the second CCX.",
                &[],
            ),
            false,
        ));
    }

    // --- Ryzen 5 6-core, modern (Zen2+) ---
    if m.contains("3600") || m.contains("5600") || m.contains("7600") || m.contains("9600") {
        return Some((
            Recommendation::from_mask("555", "Ryzen 5 6-core: one thread per physical core.", &[]),
            false,
        ));
    }

    // --- Ryzen 5 3500/3500X: 6 cores / 6 threads, no SMT ---
    if m.contains("3500") {
        return Some((
            Recommendation::no_change(
                "Ryzen 5 3500/3500X has 6 cores / 6 threads — no SMT to disable, no change needed.",
            ),
            false,
        ));
    }

    // --- Ryzen 5 6-core, Zen/Zen+ (2 CCX) ---
    if m.contains("1600") || m.contains("2600") {
        return Some((
            Recommendation::from_mask(
                "540",
                "Ryzen 5 (Zen/Zen+) 6-core (2 CCX): guide mask spanning both CCX.",
                &[],
            ),
            false,
        ));
    }

    // --- Ryzen 5 4-core / 8-thread (APUs) ---
    if m.contains("1400")
        || m.contains("1500X")
        || m.contains("2400G")
        || m.contains("2500X")
        || m.contains("3400G")
    {
        return Some((
            Recommendation::from_mask("50", "Ryzen 5 4-core / 8-thread: guide mask.", &[]),
            false,
        ));
    }

    // --- Ryzen 3 4-core / 4-thread ---
    if m.contains("1200") || m.contains("1300X") || m.contains("2200G") || m.contains("3200G") {
        return Some((
            Recommendation::from_mask("C", "Ryzen 3 4-core / 4-thread: guide mask.", &[]),
            false,
        ));
    }

    None
}

/// Match an Intel Core model against the guide's table using core counts and
/// SMT layout. Hybrid (P + E) parts and anything else the guide does not cover
/// return `None` and derive from topology instead.
fn match_intel(cpu: &CpuInfo) -> Option<(Recommendation, bool)> {
    let p = cpu.physical_cores;
    let l = cpu.logical_cores;

    // Hybrid (P + E cores, 12th gen+): logical is between physical and 2x
    // physical, because only P-cores have SMT. Not in the guide's table — the
    // topology derivation picks the P-cores by efficiency class.
    if p > 0 && l > p && l < p * 2 {
        return None;
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
            _ => return None,
        };
        return Some((rec, false));
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
        _ => return None,
    };
    Some((rec, false))
}

// ---------------------------------------------------------------------------
// Topology derivation: masks for every CPU the guide never named.
// ---------------------------------------------------------------------------

/// First logical id of each physical core, restricted to cores whose threads
/// intersect `within` (or all cores when `within` is `None`). Uses real sibling
/// data when available; otherwise assumes the x86 convention that with SMT the
/// even ids are the physical cores, and without SMT every id is one.
fn one_thread_per_core(cpu: &CpuInfo, within: Option<&[usize]>) -> Vec<usize> {
    let mut cores: Vec<usize> = if cpu.cores.is_empty() {
        let smt = cpu.physical_cores > 0 && cpu.logical_cores == cpu.physical_cores * 2;
        let all: Vec<usize> = match within {
            Some(w) => w.to_vec(),
            None => (0..cpu.logical_cores).collect(),
        };
        if smt {
            all.into_iter().filter(|c| c % 2 == 0).collect()
        } else {
            all
        }
    } else {
        cpu.cores
            .iter()
            .filter(|c| within.is_none_or(|w| c.logical_cores.iter().any(|l| w.contains(l))))
            .filter_map(|c| c.logical_cores.first().copied())
            .collect()
    };
    cores.sort_unstable();
    cores.dedup();
    cores
}

fn from_cores(
    cores: Vec<usize>,
    explanation: String,
    alternates: Vec<String>,
) -> Option<Recommendation> {
    let mask = cores_to_mask(&cores).ok()?;
    Some(Recommendation {
        mask_hex: Some(mask.to_uppercase()),
        cores,
        alternates,
        explanation,
        topology_confirmed: None,
    })
}

/// Build a recommendation from live topology alone, for CPUs the guide's table
/// does not name. Returns `None` only when no usable topology was reported.
///
/// The rules, in priority order, are the guide's own reasoning generalized:
/// hybrid parts run on the performance cores; X3D-style parts run on the
/// V-Cache CCD; multi-CCD parts stay on one CCD; SMT runs one thread per
/// physical core; and 8+ core parts leave the first core to Windows.
fn derive_from_topology(cpu: &CpuInfo) -> Option<Recommendation> {
    // --- Hybrid: multiple efficiency tiers → the fastest tier only. ---
    if let (Some(&max), Some(&min)) = (
        cpu.cores.iter().map(|c| &c.efficiency_class).max(),
        cpu.cores.iter().map(|c| &c.efficiency_class).min(),
    ) {
        if max != min {
            let fast: Vec<&crate::cpu::CoreTopo> = cpu
                .cores
                .iter()
                .filter(|c| c.efficiency_class == max)
                .collect();
            let one_each: Vec<usize> = {
                let mut v: Vec<usize> = fast
                    .iter()
                    .filter_map(|c| c.logical_cores.first().copied())
                    .collect();
                v.sort_unstable();
                v
            };
            let mut all_threads: Vec<usize> = fast
                .iter()
                .flat_map(|c| c.logical_cores.iter().copied())
                .collect();
            all_threads.sort_unstable();
            // Worth A/B testing: all P threads (HT on), and every core
            // including the efficiency ones — on parts whose E-cores are fast
            // (Arrow Lake) the exclusion is not a proven win.
            let mut alternates = Vec::new();
            for alt in [&all_threads, &one_thread_per_core(cpu, None)] {
                if *alt != one_each {
                    if let Ok(m) = cores_to_mask(alt) {
                        let m = m.to_uppercase();
                        if !alternates.contains(&m) {
                            alternates.push(m);
                        }
                    }
                }
            }
            let slow = cpu.cores.len() - fast.len();
            return from_cores(
                one_each,
                format!(
                    "Hybrid CPU ({} performance + {slow} efficiency cores, by reported \
                     efficiency class): run BDO on the performance cores, one thread per \
                     core, leaving the efficiency cores to Windows and background apps. \
                     Benchmark the alternates too — they add the remaining threads back.",
                    fast.len(),
                ),
                alternates,
            );
        }
    }

    // Hybrid-shaped core counts but no per-core data to pick P-cores with:
    // say what to do rather than guessing a mask.
    let p = cpu.physical_cores;
    let l = cpu.logical_cores;
    if cpu.cores.is_empty() && p > 0 && l > p && l < p * 2 {
        return Some(Recommendation::no_change(
            "Hybrid CPU (P + E cores). Recommendation: disable E-cores by affinity and \
             run BDO on the P-cores only. Per-core P/E topology was not available, so \
             no exact mask was generated — identify the P-core logical ids and benchmark.",
        ));
    }

    // --- X3D-style V-Cache: one L3 domain far larger than the rest. ---
    if let Some(ccd) = vcache_ccd(&cpu.l3_domains) {
        let cores = one_thread_per_core(cpu, Some(&ccd.logical_cores));
        return from_cores(
            cores,
            "L3 topology shows a V-Cache die: pin BDO to the V-Cache CCD, one thread \
             per physical core."
                .to_string(),
            Vec::new(),
        );
    }

    // --- Multiple similar L3 domains: stay on one CCD/CCX — if it is big
    // enough. Zen 1/+/2 report 2-4 small CCX domains (a Ryzen 3600 is two
    // 3-core CCXs); pinning a modern game to one of those starves it, and the
    // guide's own masks for such parts span every physical core instead. ---
    if cpu.l3_domains.len() >= 2 {
        let ccd = cpu.l3_domains.iter().max_by_key(|d| {
            (
                d.logical_cores.len(),
                std::cmp::Reverse(d.logical_cores.first().copied()),
            )
        })?;
        let cores = one_thread_per_core(cpu, Some(&ccd.logical_cores));
        let all = one_thread_per_core(cpu, None);
        if cores.len() >= 6 {
            let alternates = (all != cores)
                .then(|| cores_to_mask(&all).ok().map(|m| m.to_uppercase()))
                .flatten()
                .map(|m| vec![m])
                .unwrap_or_default();
            return from_cores(
                cores,
                format!(
                    "L3 topology shows {} core complexes: pin BDO to one complex to avoid \
                     cross-complex latency, one thread per physical core. Alternate: one \
                     thread per core across the whole CPU.",
                    cpu.l3_domains.len()
                ),
                alternates,
            );
        }
        return from_cores(
            all,
            format!(
                "L3 topology shows {} small core complexes: one complex alone is too few \
                 cores, so run one thread per physical core across all of them.",
                cpu.l3_domains.len()
            ),
            Vec::new(),
        );
    }

    // --- Homogeneous, single L3 domain: the SMT rules. ---
    let smt = if !cpu.cores.is_empty() {
        cpu.cores.iter().any(|c| c.logical_cores.len() > 1)
    } else if p > 0 && (l == p || l == p * 2) {
        l == p * 2
    } else {
        return None;
    };

    let all = one_thread_per_core(cpu, None);
    if smt {
        if all.len() >= 8 {
            let cores = all[1..].to_vec();
            let alternates = cores_to_mask(&all)
                .ok()
                .map(|m| vec![m.to_uppercase()])
                .unwrap_or_default();
            from_cores(
                cores,
                "8+ cores with SMT: one thread per physical core, first core left to \
                 Windows per the guide's pattern. Alternate: keep the first core too."
                    .to_string(),
                alternates,
            )
        } else {
            from_cores(
                all,
                "SMT CPU: one thread per physical core.".to_string(),
                Vec::new(),
            )
        }
    } else if all.len() >= 8 {
        let cores = all[2..].to_vec();
        from_cores(
            cores,
            "8+ cores without SMT: leave the first two cores to Windows, matching the \
             guide's 8-core Intel rule."
                .to_string(),
            Vec::new(),
        )
    } else {
        Some(Recommendation::no_change(
            "No SMT to disable and too few cores to set any aside — no change needed. \
             Benchmark to confirm.",
        ))
    }
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
            cores: Vec::new(),
        }
    }

    /// Build per-core topology: `(efficiency_class, smt)` per block of cores,
    /// numbered the way Windows does (P-cores first, HT siblings adjacent).
    fn topo(blocks: &[(usize, u8, bool)]) -> Vec<crate::cpu::CoreTopo> {
        let mut cores = Vec::new();
        let mut next = 0;
        for &(count, class, smt) in blocks {
            for _ in 0..count {
                let threads = if smt {
                    vec![next, next + 1]
                } else {
                    vec![next]
                };
                next += threads.len();
                cores.push(crate::cpu::CoreTopo {
                    logical_cores: threads,
                    efficiency_class: class,
                });
            }
        }
        cores
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
    fn unknown_cpu_without_topology() {
        let rec = recommend(&cpu("Totally Unknown Vendor CPU 9000", 0, 0));
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("benchmark"));
    }

    // --- Topology-derived coverage: fixtures are real market CPUs. ---

    #[test]
    fn hybrid_13900k_gets_pcore_mask() {
        // i9-13900K: 8 P-cores (HT) then 16 E-cores; P=1, E=0.
        let mut c = cpu("13th Gen Intel(R) Core(TM) i9-13900K", 24, 32);
        c.cores = topo(&[(8, 1, true), (16, 0, false)]);
        let rec = recommend(&c);
        // One thread per P-core: logical 0,2,..,14.
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10, 12, 14]);
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
        // Alternates: all P threads (FFFF), then all cores one-thread-each.
        assert_eq!(rec.alternates[0], "FFFF");
        assert!(rec.explanation.contains("performance cores"));
    }

    #[test]
    fn hybrid_14700k_gets_pcore_mask() {
        // i7-14700K: 8 P (HT) + 12 E.
        let mut c = cpu("14th Gen Intel(R) Core(TM) i7-14700K", 20, 28);
        c.cores = topo(&[(8, 1, true), (12, 0, false)]);
        let rec = recommend(&c);
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
    }

    #[test]
    fn hybrid_arrow_lake_285k_no_ht() {
        // Ultra 9 285K: 8 P + 16 E, no HyperThreading anywhere.
        let mut c = cpu("Intel(R) Core(TM) Ultra 9 285K", 24, 24);
        c.cores = topo(&[(8, 1, false), (16, 0, false)]);
        let rec = recommend(&c);
        // P-cores are logical 0..7.
        assert_eq!(rec.cores, (0..8).collect::<Vec<_>>());
        assert_eq!(rec.mask_hex.as_deref(), Some("FF"));
        // The E-cores are fast on this part; the whole-CPU mask must be
        // offered for A/B (all 24 cores).
        assert_eq!(rec.alternates, vec!["FFFFFF".to_string()]);
    }

    #[test]
    fn hybrid_12600k_gets_pcore_mask() {
        // i5-12600K: 6 P (HT) + 4 E.
        let mut c = cpu("12th Gen Intel(R) Core(TM) i5-12600K", 10, 16);
        c.cores = topo(&[(6, 1, true), (4, 0, false)]);
        let rec = recommend(&c);
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10]);
        assert_eq!(rec.mask_hex.as_deref(), Some("555"));
    }

    #[test]
    fn hybrid_without_core_data_still_advises() {
        // Hybrid-shaped counts, no per-core data: advice, not a guessed mask.
        let rec = recommend(&cpu("Intel(R) Core(TM) i9-13900K", 24, 32));
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("E-core"));
    }

    #[test]
    fn unlisted_amd_5700x_derives_5554_shape() {
        // Ryzen 7 5700X is not in the guide's table: 8c/16t, single 32MB L3.
        let mut c = cpu("AMD Ryzen 7 5700X 8-Core Processor", 8, 16);
        c.l3_domains = vec![dom(32, (0..16).collect())];
        c.cores = topo(&[(8, 0, true)]);
        let rec = recommend(&c);
        // One thread per core, first core left to Windows: 2,4,..,14 = 5554.
        assert_eq!(rec.mask_hex.as_deref(), Some("5554"));
        assert_eq!(rec.alternates, vec!["5555".to_string()]);
    }

    #[test]
    fn unlisted_5700x3d_matches_table_arm() {
        let rec = recommend(&cpu("AMD Ryzen 7 5700X3D 8-Core Processor", 8, 16));
        assert_eq!(rec.mask_hex.as_deref(), Some("5554"));
    }

    #[test]
    fn unlisted_zen2_apu_spans_small_ccxs() {
        // Ryzen 5 4600G: 6c/12t as two 3-core CCXs of 4MB each. One CCX is too
        // few cores — the mask must span all physical cores instead.
        let mut c = cpu("AMD Ryzen 5 4600G with Radeon Graphics", 6, 12);
        c.l3_domains = vec![dom(4, (0..6).collect()), dom(4, (6..12).collect())];
        c.cores = topo(&[(6, 0, true)]);
        let rec = recommend(&c);
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10]);
        assert_eq!(rec.mask_hex.as_deref(), Some("555"));
    }

    #[test]
    fn unlisted_dual_ccd_pins_one_ccd() {
        // Threadripper-style: 2x 8-core CCDs with equal 32MB L3.
        let mut c = cpu("AMD Ryzen Threadripper PRO 5945WX", 16, 32);
        c.l3_domains = vec![dom(32, (0..16).collect()), dom(32, (16..32).collect())];
        c.cores = topo(&[(16, 0, true)]);
        let rec = recommend(&c);
        // First CCD, one thread per core.
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10, 12, 14]);
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
        // Whole-CPU one-thread-per-core offered as the A/B alternate.
        assert_eq!(rec.alternates, vec!["55555555".to_string()]);
    }

    #[test]
    fn unlisted_x3d_derives_vcache_ccd() {
        // A future dual-CCD X3D the table has never heard of.
        let mut c = cpu("AMD Ryzen 9 11950X3D 16-Core Processor", 16, 32);
        c.l3_domains = vec![dom(96, (0..16).collect()), dom(32, (16..32).collect())];
        c.cores = topo(&[(16, 0, true)]);
        let rec = recommend(&c);
        assert_eq!(rec.cores, vec![0, 2, 4, 6, 8, 10, 12, 14]);
        assert_eq!(rec.mask_hex.as_deref(), Some("5555"));
        assert!(rec.explanation.contains("V-Cache"));
    }

    #[test]
    fn x3d_topology_uses_real_siblings_when_available() {
        // Same machine as `x3d_topology_overrides_table_when_vcache_is_second_ccd`
        // but with real sibling data present — result must agree.
        let mut c = cpu("AMD Ryzen 9 7950X3D 16-Core Processor", 16, 32);
        c.l3_domains = vec![dom(32, (0..16).collect()), dom(96, (16..32).collect())];
        c.cores = topo(&[(16, 0, true)]);
        let rec = recommend(&c);
        assert_eq!(rec.cores, vec![16, 18, 20, 22, 24, 26, 28, 30]);
    }

    #[test]
    fn no_smt_low_core_count_no_change() {
        // i5-9600K: 6c/6t, nothing to mask off.
        let mut c = cpu("Intel(R) Core(TM) i5-9600K CPU @ 3.70GHz", 6, 6);
        c.cores = topo(&[(6, 0, false)]);
        let rec = recommend(&c);
        assert_eq!(rec.mask_hex, None);
        assert!(rec.explanation.contains("no SMT"));
    }

    #[test]
    fn unknown_vendor_with_topology_still_derives() {
        // Whatever the brand string, topology is enough for a mask.
        let mut c = cpu("Qualcomm Snapdragon X Something", 8, 16);
        c.cores = topo(&[(8, 0, true)]);
        let rec = recommend(&c);
        assert_eq!(rec.mask_hex.as_deref(), Some("5554"));
    }

    #[test]
    fn ten_core_comet_lake_table_mask() {
        // i9-10900K: 10c/20t homogeneous — the guide's 8+ core HT arm.
        let rec = recommend(&cpu("Intel(R) Core(TM) i9-10900K CPU @ 3.70GHz", 10, 20));
        assert_eq!(rec.mask_hex.as_deref(), Some("AAA0"));
    }
}
