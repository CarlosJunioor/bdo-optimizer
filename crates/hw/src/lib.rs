//! CPU/GPU detection and BDO CPU-affinity recommendation engine.
//!
//! This crate is the pure, testable core of the BDO optimizer:
//!
//! * [`detect_cpu`] — model string, physical/logical core counts and L3 cache
//!   (CCD / X3D V-Cache) topology.
//! * [`detect_gpus`] — GPU adapters via `wgpu`, deduped with discrete GPUs
//!   first.
//! * [`recommend`] — the ACanadianDude BDO affinity table, cross-checked
//!   against live L3 topology for dual-CCD X3D parts.
//! * [`mask_to_cores`] / [`cores_to_mask`] — affinity-mask conversion helpers.
//!
//! Detection functions never panic; conversion helpers return [`Result`].

#![forbid(unsafe_op_in_unsafe_fn)]

mod cpu;
mod gpu;
mod mask;
mod recommend;
mod system;

pub use cpu::{detect_cpu, vcache_ccd, CacheInfo, CoreTopo, CpuInfo, L3Domain};
pub use gpu::{detect_gpus, GpuDeviceType, GpuInfo, GpuVendor};
pub use mask::{cores_to_mask, mask_to_cores, MaskError};
pub use recommend::{recommend, Recommendation};
pub use system::{detect_system, DiskInfo, SystemInfo};
