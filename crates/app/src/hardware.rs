//! Hardware inventory and affinity recommendation.

use egui::RichText;

use bdo_hw::{vcache_ccd_for_cpu, GpuDeviceType, GpuVendor};

use crate::app::{App, Tab};
use crate::format;
use crate::theme::{self, core_map, screen_heading, section_card};

use egui_phosphor::regular as icons;

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

        screen_heading(
            ui,
            "Inspect",
            "What this machine is, and the affinity mask it should run Black Desert on.",
        );

        section_card(ui, icons::CPU, "Processor", |ui| {
            ui.columns(2, |columns| {
                let ui = &mut columns[0];
                kv(ui, "Model", &det.cpu.model);
                kv(
                    ui,
                    "Cores / threads",
                    &format!("{} / {}", det.cpu.physical_cores, det.cpu.logical_cores),
                );
                let l3 = det
                    .cpu
                    .caches
                    .iter()
                    .find(|c| c.level == 3)
                    .map(|c| format::cache_size(c.total_size_bytes))
                    .unwrap_or_else(|| "—".into());
                kv(ui, "L3 cache", &l3);

                let ui = &mut columns[1];
                kv(ui, "Memory", &format::bytes(det.system.total_memory_bytes));
                kv(
                    ui,
                    "GPU",
                    det.gpus
                        .first()
                        .map(|g| g.name.as_str())
                        .unwrap_or("none detected"),
                );
                kv(ui, "System", &det.system.os);
            });
        });

        let continue_to_apply = self.recommendation_card(ui);

        section_card(ui, icons::STACK, "Cache topology", |ui| {
            if det.cpu.caches.is_empty() {
                ui.label(
                    RichText::new("Cache details are unavailable on this platform.")
                        .color(theme::INK_2),
                );
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

            let vcache = vcache_ccd_for_cpu(&det.cpu);
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
                                theme::ACCENT
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
                            ui.label(
                                RichText::new(format::cache_size(domain.size_bytes)).color(color),
                            );
                            ui.label(
                                RichText::new(format::cores(&domain.logical_cores)).color(color),
                            );
                            ui.end_row();
                        }
                    });
            }
        });

        section_card(ui, icons::MONITOR, "Graphics", |ui| {
            if det.gpus.is_empty() {
                ui.label(RichText::new("No GPU adapters detected.").color(theme::INK_2));
            } else {
                for gpu in &det.gpus {
                    ui.label(RichText::new(&gpu.name).strong().size(15.0));
                    ui.label(format!(
                        "{} / {} / {}",
                        vendor_str(gpu.vendor),
                        device_type_str(gpu.device_type),
                        gpu.backend
                    ));
                    if !gpu.driver.is_empty() {
                        ui.label(RichText::new(&gpu.driver).color(theme::INK_2).size(12.0));
                    }
                    ui.add_space(6.0);
                }
            }
        });

        section_card(ui, icons::HARD_DRIVES, "Storage", |ui| {
            if det.system.disks.is_empty() {
                ui.label(RichText::new("No local storage detected.").color(theme::INK_2));
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
                                    theme::OK
                                } else {
                                    theme::INK_2
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
        });

        if continue_to_apply {
            self.tab = Tab::Optimize;
        }
    }

    fn recommendation_card(&self, ui: &mut egui::Ui) -> bool {
        let Some(det) = &self.detection else {
            return false;
        };
        let recommendation = &det.recommendation;
        let mut continue_to_apply = false;
        section_card(ui, icons::PULSE, "Recommendation", |ui| {
            ui.label(RichText::new(&recommendation.explanation).color(theme::INK_2));
            match recommendation.topology_confirmed {
                Some(true) => {
                    ui.label(
                        RichText::new("Live L3 topology confirms the V-Cache CCD.")
                            .color(theme::OK),
                    );
                }
                Some(false) => {
                    ui.label(
                        RichText::new(
                            "Live topology differs from the CPU table; this mask uses the \
                             detected V-Cache CCD.",
                        )
                        .color(theme::WARN),
                    );
                }
                None => {}
            }
            ui.add_space(8.0);
            core_map(ui, &det.cpu, &recommendation.cores);
            ui.add_space(8.0);
            match &recommendation.mask_hex {
                Some(mask) => {
                    ui.horizontal(|ui| {
                        mask_badge(ui, &format!("0x{mask}"));
                        let mut caption =
                            format!("{} logical processors", recommendation.cores.len());
                        if !recommendation.alternates.is_empty() {
                            caption.push_str(&format!(
                                " · A/B test against {}",
                                recommendation
                                    .alternates
                                    .iter()
                                    .map(|mask| format!("0x{mask}"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                        ui.label(RichText::new(caption).color(theme::INK_2).size(12.5));
                    });
                    ui.add_space(6.0);
                    if ui
                        .button(RichText::new("Continue to Apply").strong())
                        .clicked()
                    {
                        continue_to_apply = true;
                    }
                }
                None => {
                    ui.label(
                        RichText::new("No mask change recommended")
                            .size(16.0)
                            .strong(),
                    );
                }
            }
        });
        continue_to_apply
    }
}

/// One key/value line in an info card.
fn kv(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).color(theme::INK_2).size(13.0));
        ui.label(RichText::new(value).size(13.5));
    });
}

/// The hex affinity mask in a bordered monospace badge.
pub(crate) fn mask_badge(ui: &mut egui::Ui, mask: &str) {
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.label(
                RichText::new(mask)
                    .monospace()
                    .size(17.0)
                    .color(theme::ACCENT),
            );
        });
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
