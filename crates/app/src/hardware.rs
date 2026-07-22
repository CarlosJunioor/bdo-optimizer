//! Hardware tab: CPU topology, GPUs, and the prominent affinity recommendation.

use egui::{Color32, RichText};

use bdo_hw::{vcache_ccd, GpuDeviceType, GpuVendor};

use crate::app::App;
use crate::format;

const ACCENT: Color32 = Color32::from_rgb(0x4d, 0xa6, 0xff);
const VCACHE: Color32 = Color32::from_rgb(0x63, 0xd6, 0x88);

impl App {
    pub(crate) fn hardware_ui(&mut self, ui: &mut egui::Ui) {
        let Some(det) = &self.detection else {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Detecting hardware…").size(16.0));
            });
            return;
        };

        // -------------------------------------------------------------- CPU
        ui.heading("CPU");
        let cpu = &det.cpu;
        egui::Grid::new("cpu_grid")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Model").strong());
                ui.label(if cpu.model.is_empty() {
                    "Unknown"
                } else {
                    &cpu.model
                });
                ui.end_row();
                ui.label(RichText::new("Physical cores").strong());
                ui.label(cpu.physical_cores.to_string());
                ui.end_row();
                ui.label(RichText::new("Logical processors").strong());
                ui.label(cpu.logical_cores.to_string());
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.label(RichText::new("L3 cache domains").strong());
        let vcache = vcache_ccd(&cpu.l3_domains);
        if cpu.l3_domains.is_empty() {
            ui.label(
                RichText::new("L3 topology unavailable on this platform.")
                    .italics()
                    .weak(),
            );
        } else {
            egui::Grid::new("l3_grid")
                .num_columns(3)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Domain").strong());
                    ui.label(RichText::new("Size").strong());
                    ui.label(RichText::new("Logical cores").strong());
                    ui.end_row();
                    for (i, dom) in cpu.l3_domains.iter().enumerate() {
                        let is_vcache = vcache == Some(dom);
                        let tag = if is_vcache {
                            format!("CCD {i} — V-Cache")
                        } else {
                            format!("CCD {i}")
                        };
                        let color = if is_vcache {
                            VCACHE
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(RichText::new(tag).color(color).strong());
                        ui.label(RichText::new(format::cache_size(dom.size_bytes)).color(color));
                        ui.label(RichText::new(format::cores(&dom.logical_cores)).color(color));
                        ui.end_row();
                    }
                });
            if vcache.is_some() {
                ui.label(
                    RichText::new(
                        "● The highlighted CCD carries the 3D V-Cache — the die BDO should run on.",
                    )
                    .color(VCACHE)
                    .size(12.0),
                );
            }
        }

        ui.add_space(12.0);
        ui.separator();

        // -------------------------------------------------------------- GPUs
        ui.heading("GPUs");
        if det.gpus.is_empty() {
            ui.label(RichText::new("No GPU adapters detected.").italics().weak());
        } else {
            egui::Grid::new("gpu_grid")
                .num_columns(3)
                .striped(true)
                .spacing([16.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Name").strong());
                    ui.label(RichText::new("Vendor").strong());
                    ui.label(RichText::new("Type").strong());
                    ui.end_row();
                    for gpu in &det.gpus {
                        ui.label(&gpu.name);
                        ui.label(vendor_str(gpu.vendor));
                        ui.label(device_type_str(gpu.device_type));
                        ui.end_row();
                    }
                });
        }

        ui.add_space(12.0);
        ui.separator();

        // ---------------------------------------------------- Recommendation
        self.recommendation_panel(ui);
    }

    fn recommendation_panel(&self, ui: &mut egui::Ui) {
        let Some(det) = &self.detection else { return };
        let rec = &det.recommendation;

        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Recommended affinity")
                        .size(20.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.add_space(4.0);

                match &rec.mask_hex {
                    Some(mask) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Mask").strong());
                            ui.label(
                                RichText::new(format!("0x{mask}"))
                                    .monospace()
                                    .size(18.0)
                                    .color(ACCENT),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Enabled cores").strong());
                            ui.label(RichText::new(format::cores(&rec.cores)).monospace());
                        });
                        if !rec.alternates.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Alternates to A/B test").strong());
                                ui.label(
                                    RichText::new(
                                        rec.alternates
                                            .iter()
                                            .map(|a| format!("0x{a}"))
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    )
                                    .monospace(),
                                );
                            });
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new("No mask change recommended for this CPU.").italics(),
                        );
                    }
                }

                ui.add_space(6.0);
                ui.label(&rec.explanation);

                ui.add_space(6.0);
                match rec.topology_confirmed {
                    Some(true) => ui.label(
                        RichText::new("✔ Live L3 topology confirmed the V-Cache CCD matches this mask.")
                            .color(VCACHE),
                    ),
                    Some(false) => ui.label(
                        RichText::new(
                            "⚠ Live topology differed from the static table — the mask above is derived from the actual V-Cache CCD.",
                        )
                        .color(Color32::from_rgb(0xff, 0xc1, 0x07)),
                    ),
                    None => ui.label(
                        RichText::new("Topology cross-check: not applicable for this CPU.")
                            .weak()
                            .size(12.0),
                    ),
                };

                ui.add_space(4.0);
                if rec.mask_hex.is_some() {
                    ui.label(
                        RichText::new("Apply this on the Optimize tab — it is pre-filled there.")
                            .size(12.0)
                            .weak(),
                    );
                }
            });
    }
}

fn vendor_str(v: GpuVendor) -> &'static str {
    match v {
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Amd => "AMD",
        GpuVendor::Intel => "Intel",
        GpuVendor::Other => "Other",
    }
}

fn device_type_str(t: GpuDeviceType) -> &'static str {
    match t {
        GpuDeviceType::Discrete => "Discrete",
        GpuDeviceType::Integrated => "Integrated",
        GpuDeviceType::Virtual => "Virtual",
        GpuDeviceType::Cpu => "CPU (software)",
        GpuDeviceType::Other => "Other",
    }
}
