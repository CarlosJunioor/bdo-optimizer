//! GPU detection via `wgpu` adapter enumeration.

/// GPU vendor derived from the PCI vendor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    /// NVIDIA (PCI vendor id `0x10DE`).
    Nvidia,
    /// AMD (PCI vendor id `0x1002`).
    Amd,
    /// Intel (PCI vendor id `0x8086`).
    Intel,
    /// Any other / unknown vendor.
    Other,
}

impl GpuVendor {
    /// Map a PCI vendor id to a [`GpuVendor`].
    pub fn from_pci_id(id: u32) -> Self {
        match id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            _ => GpuVendor::Other,
        }
    }
}

/// Coarse GPU class, used to prefer discrete GPUs in the returned list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDeviceType {
    /// Discrete GPU (dedicated card).
    Discrete,
    /// Integrated GPU (on-die / iGPU).
    Integrated,
    /// Virtual GPU (virtualized).
    Virtual,
    /// CPU-based software adapter.
    Cpu,
    /// Unknown / other device type.
    Other,
}

/// A detected GPU adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    /// Adapter/product name reported by the driver.
    pub name: String,
    /// Vendor derived from the PCI vendor id.
    pub vendor: GpuVendor,
    /// Coarse device class.
    pub device_type: GpuDeviceType,
    /// Graphics API backend used for enumeration (DX12, Vulkan, Metal, etc.).
    pub backend: String,
    /// Driver name and vendor-supplied version/details when available.
    pub driver: String,
    /// Raw PCI device id. Identifies the physical adapter across backends, which
    /// the reported name does not: the GL backend decorates it
    /// (`"NVIDIA GeForce RTX 4090/PCIe/SSE2"`), so name-keyed dedup lists one
    /// card twice, while two identical cards share a name and collapse into one.
    /// Zero when the backend does not report it.
    pub device_id: u32,
}

/// Enumerate the host GPUs across all available `wgpu` backends.
///
/// The same physical GPU can surface under several backends (e.g. Vulkan and
/// DX12); results are deduplicated by `(vendor, device_id)`, keeping the discrete
/// variant when a card appears more than once. Discrete GPUs are returned first.
///
/// Never panics; returns an empty vector when no adapters are available.
pub fn detect_gpus() -> Vec<GpuInfo> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::all();
    let instance = wgpu::Instance::new(desc);

    // `enumerate_adapters` is async for wasm parity; on native it resolves
    // immediately, so a minimal blocking poll is sufficient.
    let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::all()))
        .into_iter()
        .map(|adapter| {
            let info = adapter.get_info();
            GpuInfo {
                name: info.name.trim().to_string(),
                vendor: GpuVendor::from_pci_id(info.vendor),
                device_type: map_device_type(info.device_type),
                backend: format!("{:?}", info.backend),
                driver: [info.driver.trim(), info.driver_info.trim()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / "),
                device_id: info.device,
            }
        })
        .collect::<Vec<_>>();

    dedupe_and_sort(adapters)
}

/// Block on a future that is expected to be ready immediately (native wgpu
/// adapter enumeration). Avoids pulling in a full async runtime.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_raw_waker() -> RawWaker {
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            noop_raw_waker()
        }
        let vtable = &RawWakerVTable::new(clone, no_op, no_op, no_op);
        RawWaker::new(std::ptr::null(), vtable)
    }

    // SAFETY: the noop waker never dereferences its (null) data pointer.
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn map_device_type(t: wgpu::DeviceType) -> GpuDeviceType {
    match t {
        wgpu::DeviceType::DiscreteGpu => GpuDeviceType::Discrete,
        wgpu::DeviceType::IntegratedGpu => GpuDeviceType::Integrated,
        wgpu::DeviceType::VirtualGpu => GpuDeviceType::Virtual,
        wgpu::DeviceType::Cpu => GpuDeviceType::Cpu,
        wgpu::DeviceType::Other => GpuDeviceType::Other,
    }
}

/// Deduplicate adapters by physical identity (preferring the discrete variant)
/// and sort so discrete GPUs come first. Pure function, exercised by unit tests.
///
/// Keyed on `(vendor, device_id)` when the backend reports a device id, since the
/// same card is named differently by different backends. Falls back to the name
/// when no device id is available.
///
/// # Why the count comes from one backend, not from collapsing duplicates
///
/// `AdapterInfo::device` is the PCI *product* id, so two identical cards report
/// the same vendor and device id — collapsing every match would merge them into
/// one and under-report the machine. What distinguishes "one card seen through
/// three backends" from "three cards" is that each backend enumerates each
/// physical adapter exactly once: the real count for a model is the largest
/// number of times any single backend listed it.
fn dedupe_and_sort(adapters: Vec<GpuInfo>) -> Vec<GpuInfo> {
    fn key(g: &GpuInfo) -> (GpuVendor, u32, String) {
        if g.device_id != 0 {
            (g.vendor, g.device_id, String::new())
        } else {
            (g.vendor, 0, g.name.clone())
        }
    }

    // For each model: how many did the busiest backend report, and the best
    // representative entry seen (preferring a discrete classification).
    let mut order: Vec<(GpuVendor, u32, String)> = Vec::new();
    let mut per_backend: Vec<((GpuVendor, u32, String), String, usize)> = Vec::new();
    let mut best: Vec<((GpuVendor, u32, String), GpuInfo)> = Vec::new();

    for gpu in adapters {
        let k = key(&gpu);
        if !order.contains(&k) {
            order.push(k.clone());
        }
        match per_backend
            .iter_mut()
            .find(|(pk, backend, _)| *pk == k && *backend == gpu.backend)
        {
            Some((_, _, count)) => *count += 1,
            None => per_backend.push((k.clone(), gpu.backend.clone(), 1)),
        }
        match best.iter_mut().find(|(bk, _)| *bk == k) {
            Some((_, existing)) => {
                if gpu.device_type == GpuDeviceType::Discrete
                    && existing.device_type != GpuDeviceType::Discrete
                {
                    *existing = gpu;
                }
            }
            None => best.push((k, gpu)),
        }
    }

    let mut deduped: Vec<GpuInfo> = Vec::new();
    for k in order {
        let count = per_backend
            .iter()
            .filter(|(pk, _, _)| *pk == k)
            .map(|(_, _, c)| *c)
            .max()
            .unwrap_or(1);
        if let Some((_, gpu)) = best.iter().find(|(bk, _)| *bk == k) {
            for _ in 0..count {
                deduped.push(gpu.clone());
            }
        }
    }
    // Stable sort: discrete first, keeping original order within a class.
    deduped.sort_by_key(|g| discrete_rank(g.device_type));
    deduped
}

fn discrete_rank(t: GpuDeviceType) -> u8 {
    match t {
        GpuDeviceType::Discrete => 0,
        GpuDeviceType::Integrated => 1,
        GpuDeviceType::Virtual => 2,
        GpuDeviceType::Other => 3,
        GpuDeviceType::Cpu => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_mapping() {
        assert_eq!(GpuVendor::from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(GpuVendor::from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(GpuVendor::from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(GpuVendor::from_pci_id(0x1234), GpuVendor::Other);
    }

    fn gpu(name: &str, vendor: GpuVendor, dt: GpuDeviceType) -> GpuInfo {
        gpu_full(name, vendor, dt, 0, "Vulkan")
    }

    fn gpu_full(
        name: &str,
        vendor: GpuVendor,
        dt: GpuDeviceType,
        device_id: u32,
        backend: &str,
    ) -> GpuInfo {
        GpuInfo {
            name: name.to_string(),
            vendor,
            device_type: dt,
            backend: backend.to_string(),
            driver: "Test driver".to_string(),
            device_id,
        }
    }

    #[test]
    fn dedupe_matches_one_card_across_differently_named_backends() {
        // The GL backend decorates the name; same vendor + device id = same card.
        let input = vec![
            gpu_full(
                "RTX 4090",
                GpuVendor::Nvidia,
                GpuDeviceType::Discrete,
                0x2684,
                "Dx12",
            ),
            gpu_full(
                "NVIDIA GeForce RTX 4090/PCIe/SSE2",
                GpuVendor::Nvidia,
                GpuDeviceType::Other,
                0x2684,
                "Gl",
            ),
        ];
        let out = dedupe_and_sort(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].device_type, GpuDeviceType::Discrete);
    }

    /// Two identical cards report the *same* PCI device id — it is a product id,
    /// not a slot id — so they are told apart by one backend enumerating the
    /// model twice, not by differing ids. Keying on the id alone merged a
    /// two-card machine into one entry.
    #[test]
    fn dedupe_keeps_two_identical_cards_apart() {
        let card = |backend| {
            gpu_full(
                "RTX 4090",
                GpuVendor::Nvidia,
                GpuDeviceType::Discrete,
                0x2684,
                backend,
            )
        };
        // Two physical 4090s, each enumerated by both backends.
        let input = vec![card("Vulkan"), card("Vulkan"), card("Dx12"), card("Dx12")];
        assert_eq!(dedupe_and_sort(input).len(), 2);

        // …and one card seen by three backends is still one card.
        let input = vec![card("Vulkan"), card("Dx12"), card("Gl")];
        assert_eq!(dedupe_and_sort(input).len(), 1);
    }

    #[test]
    fn dedupe_same_gpu_across_backends() {
        // Same discrete GPU reported under Vulkan and DX12.
        let input = vec![
            gpu_full(
                "RTX 4090",
                GpuVendor::Nvidia,
                GpuDeviceType::Discrete,
                0,
                "Vulkan",
            ),
            gpu_full(
                "RTX 4090",
                GpuVendor::Nvidia,
                GpuDeviceType::Discrete,
                0,
                "Dx12",
            ),
        ];
        let out = dedupe_and_sort(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "RTX 4090");
    }

    #[test]
    fn dedupe_prefers_discrete_classification() {
        let input = vec![
            gpu_full("Weird GPU", GpuVendor::Amd, GpuDeviceType::Other, 0, "Gl"),
            gpu_full(
                "Weird GPU",
                GpuVendor::Amd,
                GpuDeviceType::Discrete,
                0,
                "Vulkan",
            ),
        ];
        let out = dedupe_and_sort(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].device_type, GpuDeviceType::Discrete);
    }

    #[test]
    fn discrete_sorted_first() {
        let input = vec![
            gpu("Intel iGPU", GpuVendor::Intel, GpuDeviceType::Integrated),
            gpu("RTX 4090", GpuVendor::Nvidia, GpuDeviceType::Discrete),
        ];
        let out = dedupe_and_sort(input);
        assert_eq!(out[0].name, "RTX 4090");
        assert_eq!(out[1].name, "Intel iGPU");
    }
}
