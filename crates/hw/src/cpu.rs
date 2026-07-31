//! CPU detection and L3 cache-topology (CCD / X3D V-Cache) discovery.

/// A single L3 cache domain and the logical processors that share it.
///
/// On AMD chips each Core Complex Die (CCD) has its own L3 slice, so one
/// `L3Domain` maps to one CCD. On X3D dual-CCD parts the V-Cache die reports a
/// much larger `size_bytes` (~96 MB) than the plain die (~32 MB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L3Domain {
    /// Size of this L3 slice in bytes.
    pub size_bytes: u64,
    /// Logical processor indices that share this L3 slice.
    pub logical_cores: Vec<usize>,
}

/// Total cache capacity reported for one CPU cache level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheInfo {
    pub level: u8,
    pub total_size_bytes: u64,
    pub instances: usize,
}

/// One physical core: its SMT threads and its performance tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreTopo {
    /// Logical processor ids belonging to this physical core (1 without SMT,
    /// 2 with). Sorted ascending.
    pub logical_cores: Vec<usize>,
    /// Windows `EfficiencyClass`: higher is faster. On Intel hybrid parts
    /// P-cores report a higher class than E-cores; on homogeneous CPUs every
    /// core reports the same value. Always 0 where the OS has no such notion.
    pub efficiency_class: u8,
}

/// Detected CPU characteristics used by the recommendation engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuInfo {
    /// Marketing model string, e.g. `"AMD Ryzen 9 7950X3D 16-Core Processor"`.
    pub model: String,
    /// Number of physical cores.
    pub physical_cores: usize,
    /// Number of logical processors (physical cores x SMT threads).
    pub logical_cores: usize,
    /// L3 cache domains (one per CCD on AMD). Empty when topology is
    /// unavailable (e.g. macOS).
    pub l3_domains: Vec<L3Domain>,
    /// Total capacity and instance count for each reported cache level.
    pub caches: Vec<CacheInfo>,
    /// Per-physical-core topology (SMT siblings + efficiency class), sorted by
    /// first logical id. Empty when unavailable.
    pub cores: Vec<CoreTopo>,
}

/// Detect the host CPU: model string, core counts and L3 topology.
///
/// Never panics; falls back to best-effort values when a source is
/// unavailable.
pub fn detect_cpu() -> CpuInfo {
    use sysinfo::System;

    let mut sys = System::new();
    sys.refresh_cpu_all();

    let logical_cores = sys.cpus().len();

    let mut model = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();

    // Prefer the CPUID brand string on x86 when sysinfo gives us nothing useful.
    #[cfg(target_arch = "x86_64")]
    if model.is_empty() {
        if let Some(brand) = cpuid_brand() {
            model = brand;
        }
    }

    let physical_cores = System::physical_core_count().unwrap_or(logical_cores.max(1));

    let (l3_domains, caches) = detect_caches();
    let mut cores = detect_core_topology();
    cores.sort_by_key(|c| c.logical_cores.first().copied().unwrap_or(usize::MAX));
    CpuInfo {
        model,
        physical_cores,
        logical_cores,
        l3_domains,
        caches,
        cores,
    }
}

#[cfg(target_arch = "x86_64")]
fn cpuid_brand() -> Option<String> {
    use raw_cpuid::CpuId;
    let cpuid = CpuId::new();
    cpuid
        .get_processor_brand_string()
        .map(|b| b.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Identify the V-Cache CCD among a set of L3 domains.
///
/// Returns the largest L3 domain when there are >= 2 domains and the largest is
/// at least twice the size of the next-largest (the X3D signature: ~96 MB vs
/// ~32 MB). Returns `None` for single-CCD parts or when the split is not
/// significant.
pub fn vcache_ccd(domains: &[L3Domain]) -> Option<&L3Domain> {
    if domains.len() < 2 {
        return None;
    }
    let mut sorted: Vec<&L3Domain> = domains.iter().collect();
    sorted.sort_by_key(|d| std::cmp::Reverse(d.size_bytes));
    if sorted[0].size_bytes >= sorted[1].size_bytes.saturating_mul(2) {
        Some(sorted[0])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Windows: GetLogicalProcessorInformationEx(RelationProcessorCore)
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn detect_core_topology() -> Vec<CoreTopo> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore, GROUP_AFFINITY,
        LOGICAL_PROCESSOR_RELATIONSHIP, PROCESSOR_RELATIONSHIP,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    let mut cores = Vec::new();
    unsafe {
        let mut len: u32 = 0;
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
        if len == 0 {
            return cores;
        }

        // u64-backed buffer for the same alignment reason as `detect_caches`.
        let mut buffer: Vec<u64> = vec![0; (len as usize).div_ceil(8)];
        let ptr = buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
        if GetLogicalProcessorInformationEx(RelationProcessorCore, Some(ptr), &mut len).is_err() {
            return cores;
        }

        // Same variable-length walk as `detect_caches`: never materialise a
        // reference to the full union, read each variant unaligned.
        const HEADER: usize = 2 * std::mem::size_of::<u32>();
        let min_record = HEADER + std::mem::size_of::<PROCESSOR_RELATIONSHIP>();
        let base = buffer.as_ptr() as *const u8;
        let len = len as usize;

        let mut offset: usize = 0;
        while offset + HEADER <= len {
            let size = std::ptr::read_unaligned(
                base.add(offset + std::mem::size_of::<u32>()) as *const u32
            ) as usize;
            if size < min_record || len - offset < size {
                break;
            }

            let relationship =
                std::ptr::read_unaligned(base.add(offset) as *const LOGICAL_PROCESSOR_RELATIONSHIP);
            if relationship == RelationProcessorCore {
                let core = std::ptr::read_unaligned(
                    base.add(offset + HEADER) as *const PROCESSOR_RELATIONSHIP
                );
                // The struct declares one GROUP_AFFINITY but `GroupCount` of
                // them actually follow; walk them all so >64-thread machines
                // (Threadripper) are not silently truncated to group 0.
                let masks_at =
                    offset + HEADER + std::mem::offset_of!(PROCESSOR_RELATIONSHIP, GroupMask);
                let ga_size = std::mem::size_of::<GROUP_AFFINITY>();
                let mut logical = Vec::new();
                for i in 0..core.GroupCount.max(1) as usize {
                    let at = masks_at + i * ga_size;
                    if at + ga_size > offset + size {
                        break;
                    }
                    let ga = std::ptr::read_unaligned(base.add(at) as *const GROUP_AFFINITY);
                    let group_base = ga.Group as usize * 64;
                    let mask = ga.Mask as u64;
                    logical.extend(
                        (0..64u32)
                            .filter(|b| mask & (1u64 << b) != 0)
                            .map(|b| group_base + b as usize),
                    );
                }
                logical.sort_unstable();
                if !logical.is_empty() {
                    cores.push(CoreTopo {
                        logical_cores: logical,
                        efficiency_class: core.EfficiencyClass,
                    });
                }
            }

            offset += size;
        }
    }
    cores
}

// ---------------------------------------------------------------------------
// Linux: /sys/devices/system/cpu/cpu*/topology/
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn detect_core_topology() -> Vec<CoreTopo> {
    use std::collections::BTreeSet;

    let mut seen: BTreeSet<Vec<usize>> = BTreeSet::new();
    let mut cores = Vec::new();
    let Ok(root) = std::fs::read_dir("/sys/devices/system/cpu") else {
        return cores;
    };
    for entry in root.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let topo = entry.path().join("topology");
        let list = read_trimmed(&topo.join("core_cpus_list"))
            .or_else(|| read_trimmed(&topo.join("thread_siblings_list")));
        let Some(list) = list.as_deref().and_then(parse_cpu_list) else {
            continue;
        };
        if list.is_empty() || !seen.insert(list.clone()) {
            continue;
        }
        // ponytail: efficiency_class stays 0 on Linux — sysfs has no direct
        // EfficiencyClass equivalent and the affinity workflow is Windows-only.
        cores.push(CoreTopo {
            logical_cores: list,
            efficiency_class: 0,
        });
    }
    cores
}

#[cfg(not(any(windows, target_os = "linux")))]
fn detect_core_topology() -> Vec<CoreTopo> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Windows: GetLogicalProcessorInformationEx(RelationCache)
// ---------------------------------------------------------------------------
#[cfg(windows)]
fn detect_caches() -> (Vec<L3Domain>, Vec<CacheInfo>) {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationCache, CACHE_RELATIONSHIP,
        LOGICAL_PROCESSOR_RELATIONSHIP, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    let mut domains = Vec::new();
    let mut levels = std::collections::BTreeMap::<u8, (u64, usize)>::new();

    unsafe {
        // First call: query required buffer length.
        let mut len: u32 = 0;
        let _ = GetLogicalProcessorInformationEx(RelationCache, None, &mut len);
        if len == 0 {
            return (domains, Vec::new());
        }

        // Back the buffer with u64: SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX needs
        // 8-byte alignment and a Vec<u8> only guarantees 1.
        let mut buffer: Vec<u64> = vec![0; (len as usize).div_ceil(8)];
        let ptr = buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
        if GetLogicalProcessorInformationEx(RelationCache, Some(ptr), &mut len).is_err() {
            return (domains, Vec::new());
        }

        // Records are variable-length; advance by each record's `Size`. Every
        // record begins with Relationship (u32) then Size (u32), and the union
        // follows at HEADER.
        //
        // We read the Cache variant through `read_unaligned` rather than
        // materialising a `&SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX`: a reference
        // asserts the whole 80-byte struct is readable (the union is sized by its
        // largest variant, GROUP_RELATIONSHIP), but real cache records are 56
        // bytes, so the final record's reference would run past the buffer.
        const HEADER: usize = 2 * std::mem::size_of::<u32>();
        let min_record = HEADER + std::mem::size_of::<CACHE_RELATIONSHIP>();
        let base = buffer.as_ptr() as *const u8;
        let len = len as usize;

        let mut offset: usize = 0;
        while offset + HEADER <= len {
            let size = std::ptr::read_unaligned(
                base.add(offset + std::mem::size_of::<u32>()) as *const u32
            ) as usize;
            // The record must lie wholly inside the buffer and be large enough to
            // hold the variant we are about to read. Anything else (including the
            // Size == 0 terminator) ends the walk.
            if size < min_record || len - offset < size {
                break;
            }

            let relationship =
                std::ptr::read_unaligned(base.add(offset) as *const LOGICAL_PROCESSOR_RELATIONSHIP);
            if relationship == RelationCache {
                let cache = std::ptr::read_unaligned(
                    base.add(offset + HEADER) as *const CACHE_RELATIONSHIP
                );
                let group = cache.Anonymous.GroupMask;
                let base = group.Group as usize * 64;
                let mask = group.Mask as u64;
                let cores: Vec<usize> = (0..64u32)
                    .filter(|i| mask & (1u64 << i) != 0)
                    .map(|i| base + i as usize)
                    .collect();
                if !cores.is_empty() {
                    let level = cache.Level;
                    let summary = levels.entry(level).or_default();
                    summary.0 += cache.CacheSize as u64;
                    summary.1 += 1;
                    if level == 3 {
                        domains.push(L3Domain {
                            size_bytes: cache.CacheSize as u64,
                            logical_cores: cores,
                        });
                    }
                }
            }

            offset += size;
        }
    }

    (
        domains,
        levels
            .into_iter()
            .map(|(level, (total_size_bytes, instances))| CacheInfo {
                level,
                total_size_bytes,
                instances,
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Linux: /sys/devices/system/cpu/cpu*/cache/index*/
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn detect_caches() -> (Vec<L3Domain>, Vec<CacheInfo>) {
    use std::collections::BTreeSet;
    use std::fs;

    let mut domains: Vec<L3Domain> = Vec::new();
    let mut levels = std::collections::BTreeMap::<u8, (u64, usize)>::new();
    let mut seen: BTreeSet<(u8, String, Vec<usize>)> = BTreeSet::new();

    let cpu_root = match fs::read_dir("/sys/devices/system/cpu") {
        Ok(d) => d,
        Err(_) => return (domains, Vec::new()),
    };

    for cpu_entry in cpu_root.flatten() {
        let name = cpu_entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let cache_dir = cpu_entry.path().join("cache");
        let indices = match fs::read_dir(&cache_dir) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for index_entry in indices.flatten() {
            let path = index_entry.path();
            let Some(level) = read_trimmed(&path.join("level")).and_then(|v| v.parse().ok()) else {
                continue;
            };
            let cache_type = read_trimmed(&path.join("type")).unwrap_or_default();
            let cores = match read_trimmed(&path.join("shared_cpu_list"))
                .and_then(|s| parse_cpu_list(&s))
            {
                Some(c) if !c.is_empty() => c,
                _ => continue,
            };
            if !seen.insert((level, cache_type, cores.clone())) {
                continue;
            }
            let size_bytes = read_trimmed(&path.join("size"))
                .and_then(|s| parse_cache_size(&s))
                .unwrap_or(0);
            let summary = levels.entry(level).or_default();
            summary.0 += size_bytes;
            summary.1 += 1;
            if level == 3 {
                domains.push(L3Domain {
                    size_bytes,
                    logical_cores: cores,
                });
            }
        }
    }

    (
        domains,
        levels
            .into_iter()
            .map(|(level, (total_size_bytes, instances))| CacheInfo {
                level,
                total_size_bytes,
                instances,
            })
            .collect(),
    )
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Parse a Linux `shared_cpu_list` string like `"0-7"` or `"0,2,4"` or
/// `"0-3,8-11"` into sorted logical core indices.
#[cfg(target_os = "linux")]
fn parse_cpu_list(s: &str) -> Option<Vec<usize>> {
    let mut cores = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: usize = a.trim().parse().ok()?;
            let end: usize = b.trim().parse().ok()?;
            for c in start..=end {
                cores.push(c);
            }
        } else {
            cores.push(part.parse().ok()?);
        }
    }
    cores.sort_unstable();
    cores.dedup();
    Some(cores)
}

/// Parse a Linux cache `size` string like `"32768K"`, `"96M"` into bytes.
#[cfg(target_os = "linux")]
fn parse_cache_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix(['K', 'k']) {
        (n, 1024)
    } else if let Some(n) = s.strip_suffix(['M', 'm']) {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix(['G', 'g']) {
        (n, 1024 * 1024 * 1024)
    } else {
        (s, 1)
    };
    num.trim().parse::<u64>().ok().map(|v| v * mult)
}

// ---------------------------------------------------------------------------
// macOS / other: no portable per-cache topology API; degrade gracefully.
// ---------------------------------------------------------------------------
#[cfg(not(any(windows, target_os = "linux")))]
fn detect_caches() -> (Vec<L3Domain>, Vec<CacheInfo>) {
    (Vec::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dom(size_mb: u64, cores: Vec<usize>) -> L3Domain {
        L3Domain {
            size_bytes: size_mb * 1024 * 1024,
            logical_cores: cores,
        }
    }

    #[test]
    fn vcache_none_for_single_domain() {
        let domains = vec![dom(32, vec![0, 1, 2, 3])];
        assert!(vcache_ccd(&domains).is_none());
    }

    #[test]
    fn vcache_none_for_empty() {
        assert!(vcache_ccd(&[]).is_none());
    }

    #[test]
    fn vcache_none_for_equal_domains() {
        let domains = vec![
            dom(32, vec![0, 2, 4, 6, 8, 10, 12, 14]),
            dom(32, vec![16, 18, 20, 22, 24, 26, 28, 30]),
        ];
        assert!(vcache_ccd(&domains).is_none());
    }

    #[test]
    fn vcache_detects_larger_die() {
        // 7950X3D-like: 96 MB V-Cache CCD + 32 MB plain CCD.
        let vcache = dom(
            96,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        );
        let plain = dom(
            32,
            vec![
                16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            ],
        );
        let domains = vec![plain, vcache.clone()];
        let found = vcache_ccd(&domains).expect("should find v-cache CCD");
        assert_eq!(found, &vcache);
    }

    #[test]
    fn vcache_needs_at_least_2x() {
        // 48 MB vs 32 MB is only 1.5x -> not a clear X3D split.
        let domains = vec![dom(48, vec![0, 1]), dom(32, vec![2, 3])];
        assert!(vcache_ccd(&domains).is_none());
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn live_core_topology_is_consistent() {
        // Runs against the real machine (and CI): every logical processor
        // belongs to exactly one physical core, 1-2 threads each.
        let cpu = detect_cpu();
        assert!(
            !cpu.cores.is_empty(),
            "core topology must be available here"
        );
        let mut seen: Vec<usize> = cpu
            .cores
            .iter()
            .flat_map(|c| c.logical_cores.iter().copied())
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..cpu.logical_cores).collect::<Vec<_>>());
        assert!(cpu
            .cores
            .iter()
            .all(|c| (1..=2).contains(&c.logical_cores.len())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_cpu_list_ranges() {
        assert_eq!(parse_cpu_list("0-7").unwrap(), (0..=7).collect::<Vec<_>>());
        assert_eq!(parse_cpu_list("0,2,4").unwrap(), vec![0, 2, 4]);
        assert_eq!(parse_cpu_list("0-3,8-9").unwrap(), vec![0, 1, 2, 3, 8, 9]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_cache_size_units() {
        assert_eq!(parse_cache_size("32768K"), Some(32768 * 1024));
        assert_eq!(parse_cache_size("96M"), Some(96 * 1024 * 1024));
        assert_eq!(parse_cache_size("1024"), Some(1024));
    }
}
