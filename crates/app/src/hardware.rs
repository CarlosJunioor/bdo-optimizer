//! Hardware inventory and affinity recommendation.

use egui::{Color32, RichText};

use bdo_hw::{vcache_ccd, GpuDeviceType, GpuVendor};

use crate::app::App;
use crate::format;

const ACCENT: Color32 = Color32::from_rgb(104, 200, 255);
const VCACHE: Color32 = Color32::from_rgb(99, 214, 136);
const MUTED: Color32 = Color32::from_rgb(150, 166, 186);
const SURFACE: Color32 = Color32::from_rgb(16, 25, 39);
const BORDER: Color32 = Color32::from_rgb(42, 59, 79);

impl App {
    pub(crate) fn hardware_ui(&mut self, ui: &mut egui::Ui) {
        let Some(det) = &self.detection else {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading this PC...").size(16.0));
            });
            return;
        };

        ui.label(RichText::new("This PC").size(28.0).strong());
        ui.label(
            RichText::new("The hardware facts that affect an affinity benchmark.")
                .color(MUTED)
                .size(13.0),
        );
        ui.add_space(10.0);

        ui.columns(3, |columns| {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(14))
                .show(&mut columns[0], |ui| {
                    ui.label(
                        RichText::new("PROCESSOR")
                            .monospace()
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.label(RichText::new(&det.cpu.model).strong().size(15.0));
                    ui.label(format!(
                        "{} cores / {} threads",
                        det.cpu.physical_cores, det.cpu.logical_cores
                    ));
                });
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(14))
                .show(&mut columns[1], |ui| {
                    ui.label(RichText::new("MEMORY").monospace().size(11.0).color(MUTED));
                    ui.label(
                        RichText::new(format::bytes(det.system.total_memory_bytes))
                            .strong()
                            .size(22.0)
                            .color(ACCENT),
                    );
                    ui.label(format!(
                        "{} available now",
                        format::bytes(det.system.available_memory_bytes)
                    ));
                });
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(14))
                .show(&mut columns[2], |ui| {
                    ui.label(RichText::new("SYSTEM").monospace().size(11.0).color(MUTED));
                    ui.label(RichText::new(&det.system.os).strong().size(15.0));
                    ui.label(format!(
                        "{} GPU{} / {} drive{}",
                        det.gpus.len(),
                        if det.gpus.len() == 1 { "" } else { "s" },
                        det.system.disks.len(),
                        if det.system.disks.len() == 1 { "" } else { "s" }
                    ));
                });
        });

        ui.add_space(14.0);
        self.recommendation_panel(ui);
        ui.add_space(14.0);

        ui.label(RichText::new("CPU cache topology").size(20.0).strong());
        if det.cpu.caches.is_empty() {
            ui.label(RichText::new("Cache details are unavailable on this platform.").color(MUTED));
        } else {
            egui::Grid::new("cache_summary")
                .num_columns(3)
                .striped(true)
                .spacing([28.0, 8.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Level").strong());
                    ui.label(RichText::new("Total capacity").strong());
                    ui.label(RichText::new("Cache records").strong());
                    ui.end_row();
                    for cache in &det.cpu.caches {
                        ui.label(
                            RichText::new(format!("L{}", cache.level))
                                .monospace()
                                .strong(),
                        );
                        ui.label(format::cache_size(cache.total_size_bytes));
                        ui.label(cache.instances.to_string());
                        ui.end_row();
                    }
                });
        }

        let vcache = vcache_ccd(&det.cpu.l3_domains);
        if !det.cpu.l3_domains.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("L3 domains / CCDs").strong());
            egui::Grid::new("l3_grid")
                .num_columns(3)
                .striped(true)
                .spacing([28.0, 8.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Domain").strong());
                    ui.label(RichText::new("Capacity").strong());
                    ui.label(RichText::new("Logical processors").strong());
                    ui.end_row();
                    for (i, domain) in det.cpu.l3_domains.iter().enumerate() {
                        let is_vcache = vcache == Some(domain);
                        let color = if is_vcache {
                            VCACHE
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(
                            RichText::new(if is_vcache {
                                format!("CCD {i} / 3D V-Cache")
                            } else {
                                format!("CCD {i}")
                            })
                            .color(color)
                            .strong(),
                        );
                        ui.label(RichText::new(format::cache_size(domain.size_bytes)).color(color));
                        ui.label(RichText::new(format::cores(&domain.logical_cores)).color(color));
                        ui.end_row();
                    }
                });
        }

        ui.add_space(18.0);
        ui.separator();
        ui.add_space(10.0);
        ui.label(RichText::new("Graphics").size(20.0).strong());
        if det.gpus.is_empty() {
            ui.label(RichText::new("No GPU adapters detected.").color(MUTED));
        } else {
            for gpu in &det.gpus {
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&gpu.name).strong().size(16.0));
                        ui.label(format!(
                            "{} / {} / {}",
                            vendor_str(gpu.vendor),
                            device_type_str(gpu.device_type),
                            gpu.backend
                        ));
                        if !gpu.driver.is_empty() {
                            ui.label(RichText::new(&gpu.driver).color(MUTED).size(12.0));
                        }
                    });
            }
        }

        ui.add_space(18.0);
        ui.separator();
        ui.add_space(10.0);
        ui.label(RichText::new("Storage").size(20.0).strong());
        if det.system.disks.is_empty() {
            ui.label(RichText::new("No local storage detected.").color(MUTED));
        } else {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                egui::Grid::new("storage_grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([20.0, 8.0])
                    .show(ui, |ui| {
                        for heading in [
                            "Drive",
                            "Type",
                            "Capacity",
                            "Available",
                            "File system",
                            "Mount",
                        ] {
                            ui.label(RichText::new(heading).strong());
                        }
                        ui.end_row();
                        for disk in &det.system.disks {
                            ui.label(if disk.name.is_empty() {
                                "Local disk"
                            } else {
                                &disk.name
                            });
                            ui.label(RichText::new(&disk.kind).color(if disk.kind == "SSD" {
                                VCACHE
                            } else {
                                MUTED
                            }));
                            ui.label(format::bytes(disk.total_bytes));
                            ui.label(format::bytes(disk.available_bytes));
                            ui.label(&disk.file_system);
                            ui.label(RichText::new(&disk.mount_point).monospace().size(12.0));
                            ui.end_row();
                        }
                    });
            });
        }
    }

    fn recommendation_panel(&self, ui: &mut egui::Ui) {
        let Some(det) = &self.detection else { return };
        let recommendation = &det.recommendation;
        egui::Frame::new()
            .fill(Color32::from_rgb(14, 37, 53))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(35, 117, 156)))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("RECOMMENDED AFFINITY")
                        .monospace()
                        .size(11.0)
                        .color(ACCENT),
                );
                match &recommendation.mask_hex {
                    Some(mask) => {
                        ui.label(
                            RichText::new(format!("0x{mask}"))
                                .monospace()
                                .size(30.0)
                                .strong()
                                .color(ACCENT),
                        );
                        ui.label(format!(
                            "Logical processors: {}",
                            format::cores(&recommendation.cores)
                        ));
                        if !recommendation.alternates.is_empty() {
                            ui.label(format!(
                                "A/B test against: {}",
                                recommendation
                                    .alternates
                                    .iter()
                                    .map(|mask| format!("0x{mask}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new("No mask change recommended")
                                .size(20.0)
                                .strong(),
                        );
                    }
                }
                ui.add_space(4.0);
                ui.label(&recommendation.explanation);
                match recommendation.topology_confirmed {
                    Some(true) => {
                        ui.label(RichText::new("Live L3 topology confirms the V-Cache CCD.").color(VCACHE));
                    }
                    Some(false) => {
                        ui.label(
                            RichText::new("Live topology differs from the CPU table; this mask uses the detected V-Cache CCD.")
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                    None => {}
                }
                if recommendation.mask_hex.is_some() {
                    ui.label(
                        RichText::new("The Optimize tab is pre-filled with this result.")
                            .color(MUTED)
                            .size(12.0),
                    );
                }
            });
    }
}

fn vendor_str(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Amd => "AMD",
        GpuVendor::Intel => "Intel",
        GpuVendor::Other => "Other",
    }
}

fn device_type_str(device_type: GpuDeviceType) -> &'static str {
    match device_type {
        GpuDeviceType::Discrete => "Discrete",
        GpuDeviceType::Integrated => "Integrated",
        GpuDeviceType::Virtual => "Virtual",
        GpuDeviceType::Cpu => "Software",
        GpuDeviceType::Other => "Other",
    }
}
