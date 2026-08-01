//! Design tokens and shared visual building blocks.
//!
//! One place owns the look: the desert-night palette, the three embedded fonts
//! (Chakra Petch for display, IBM Plex Sans for body, IBM Plex Mono for data),
//! the card frame every section lives in, and the core-map widget that draws
//! an affinity mask as physical core chips. Everything else styles itself by
//! calling into here, so a palette change is a one-file edit.

use egui::{Color32, CornerRadius, FontFamily, FontId, RichText, Stroke, TextStyle};

// ---------------------------------------------------------------------------
// Palette — warm ink and one ember-gold accent on cool dark surfaces.
// ---------------------------------------------------------------------------
pub const BG: Color32 = Color32::from_rgb(0x0C, 0x0F, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x10, 0x14, 0x1D);
pub const CARD: Color32 = Color32::from_rgb(0x16, 0x1B, 0x26);
pub const HOVER: Color32 = Color32::from_rgb(0x1D, 0x23, 0x30);
pub const STROKE: Color32 = Color32::from_rgb(0x25, 0x2C, 0x3B);
pub const STROKE_SOFT: Color32 = Color32::from_rgb(0x1C, 0x22, 0x30);

pub const INK: Color32 = Color32::from_rgb(0xE7, 0xE2, 0xD6);
pub const INK_2: Color32 = Color32::from_rgb(0x9B, 0x97, 0x8C);
pub const INK_3: Color32 = Color32::from_rgb(0x6A, 0x69, 0x63);

pub const ACCENT: Color32 = Color32::from_rgb(0xE5, 0xA8, 0x3E);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x8A, 0x65, 0x24);
/// Chart-mark gold — darker than [`ACCENT`]; validated for contrast and CVD
/// separation against [`CARD`] together with [`CHART_2`].
pub const CHART_1: Color32 = Color32::from_rgb(0xBD, 0x82, 0x26);
pub const CHART_2: Color32 = Color32::from_rgb(0x5E, 0x8F, 0xD9);
pub const CHART_3: Color32 = Color32::from_rgb(0xC9, 0x6A, 0x94);

pub const OK: Color32 = Color32::from_rgb(0x66, 0xBD, 0x8C);
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0xC5, 0x58);
pub const ERR: Color32 = Color32::from_rgb(0xE0, 0x60, 0x56);

/// The display font family (Chakra Petch), registered by [`apply`].
pub fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

/// Install fonts and the app-wide style. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "plex-sans".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        "plex-mono".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        "chakra".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf"))
            .into(),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "plex-sans".into());
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .unwrap()
        .insert(0, "plex-mono".into());
    fonts
        .families
        .insert(display(), vec!["chakra".into(), "plex-sans".into()]);
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(22.0, display())),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size = egui::vec2(44.0, 34.0);

    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = PANEL;
    v.faint_bg_color = HOVER;
    v.override_text_color = Some(INK);
    v.widgets.noninteractive.bg_stroke.color = STROKE_SOFT;
    v.widgets.noninteractive.fg_stroke.color = INK;
    v.widgets.inactive.weak_bg_fill = HOVER;
    v.widgets.inactive.bg_fill = HOVER;
    v.widgets.inactive.fg_stroke.color = INK;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x25, 0x2C, 0x3C);
    v.widgets.hovered.bg_stroke.color = STROKE;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERR;
    // Hand cursor on every hoverable widget (buttons, nav rows, links) —
    // without this egui leaves the arrow cursor and buttons feel dead.
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(9);
    v.widgets.inactive.corner_radius = CornerRadius::same(9);
    v.widgets.hovered.corner_radius = CornerRadius::same(9);
    v.widgets.active.corner_radius = CornerRadius::same(9);
    ctx.set_style_of(egui::Theme::Dark, style);
}

// ---------------------------------------------------------------------------
// Layout building blocks
// ---------------------------------------------------------------------------

/// Content column: full width up to 860 px, centered, with breathing room.
pub fn content_column<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let max = ui.max_rect();
    let width = (max.width() - 64.0).clamp(280.0, 860.0);
    let x0 = max.center().x - width / 2.0;
    let rect = egui::Rect::from_min_max(
        egui::pos2(x0, max.top() + 28.0),
        egui::pos2(x0 + width, max.bottom()),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let r = add(&mut child);
    // Reserve what the child actually used, plus bottom padding, so the
    // enclosing scroll area knows the real content height.
    ui.allocate_rect(
        egui::Rect::from_min_max(
            max.min,
            egui::pos2(max.right(), child.min_rect().bottom() + 48.0),
        ),
        egui::Sense::hover(),
    );
    r
}

/// A section card: rounded frame, phosphor icon + Chakra Petch title, body.
pub fn section_card<R>(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(icon).size(16.0).color(ACCENT));
            ui.label(
                RichText::new(title)
                    .font(FontId::new(15.0, display()))
                    .color(INK),
            );
        });
        ui.add_space(4.0);
        add(ui)
    })
}

/// A bare card frame with the standard fill, stroke, radius and padding.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, STROKE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(8.0);
    r
}

/// Screen heading: Chakra Petch title plus a weak one-line subtitle.
pub fn screen_heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(
        RichText::new(title)
            .text_style(TextStyle::Heading)
            .color(INK),
    );
    ui.label(RichText::new(subtitle).color(INK_2));
    ui.add_space(14.0);
}

// ---------------------------------------------------------------------------
// Core map — an affinity mask drawn as physical core chips, grouped by CCD.
// ---------------------------------------------------------------------------

/// Draw the CPU as physical-core chips; a chip is gold when any of its logical
/// processors is in `lit`, and each of its thread dots lights individually.
/// Groups follow L3 domains (CCDs) when there are several; the V-Cache die is
/// tagged. Falls back to one untitled group and even/odd siblings when
/// topology is missing.
pub fn core_map(ui: &mut egui::Ui, cpu: &bdo_hw::CpuInfo, lit: &[usize]) {
    let cores: Vec<Vec<usize>> = if cpu.cores.is_empty() {
        let smt = cpu.physical_cores > 0 && cpu.logical_cores == cpu.physical_cores * 2;
        (0..cpu.physical_cores.max(1))
            .map(|c| if smt { vec![c * 2, c * 2 + 1] } else { vec![c] })
            .collect()
    } else {
        cpu.cores.iter().map(|c| c.logical_cores.clone()).collect()
    };

    // Partition physical cores into L3 groups (a core belongs where its first
    // thread lives); one group when the topology reports fewer than two.
    let vcache_first = bdo_hw::vcache_ccd(&cpu.l3_domains).map(|d| d.logical_cores.clone());
    let groups: Vec<(String, bool, Vec<&Vec<usize>>)> = if cpu.l3_domains.len() >= 2 {
        let mut domains: Vec<_> = cpu.l3_domains.iter().collect();
        domains.sort_by_key(|d| d.logical_cores.first().copied());
        domains
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let vcache = vcache_first.as_deref() == Some(&d.logical_cores);
                let label = format!("CCD {i} · {} MB", d.size_bytes / (1024 * 1024));
                let members = cores
                    .iter()
                    .filter(|c| c.iter().any(|l| d.logical_cores.contains(l)))
                    .collect();
                (label, vcache, members)
            })
            .collect()
    } else {
        vec![(String::new(), false, cores.iter().collect())]
    };

    // Keep repainting while any core is lit so the activity pulse animates.
    // core_map only runs while it is on screen, so this costs nothing elsewhere.
    if !lit.is_empty() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }

    ui.horizontal_wrapped(|ui| {
        for (label, vcache, members) in &groups {
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0, STROKE))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    if !label.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(label).small().color(INK_2));
                            if *vcache {
                                ui.label(RichText::new("V-CACHE").small().color(ACCENT));
                            }
                        });
                        ui.add_space(6.0);
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for (index, threads) in cores.iter().enumerate() {
                            if !members.iter().any(|m| std::ptr::eq(*m, threads)) {
                                continue;
                            }
                            chip(ui, index, threads, lit);
                        }
                    });
                });
        }
    });

    if !lit.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
            let pulse = pulse_at(ui, 0);
            ui.painter().circle_filled(
                dot.center(),
                2.5 + 1.0 * pulse,
                lerp_color(ACCENT_DIM, ACCENT, pulse),
            );
            ui.label(
                RichText::new("The game runs on the glowing cores — dim ones stay free for Windows.")
                    .small()
                    .color(INK_3),
            );
        });
    }
}

/// 0..1 activity pulse, phase-shifted per core so the glow sweeps across the map.
fn pulse_at(ui: &egui::Ui, index: usize) -> f32 {
    let t = ui.input(|i| i.time);
    (((t * 2.2 - index as f64 * 0.45).sin() * 0.5 + 0.5) as f32).powi(2)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn chip(ui: &mut egui::Ui, index: usize, threads: &[usize], lit: &[usize]) {
    let on = threads.iter().any(|t| lit.contains(t));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 34.0), egui::Sense::hover());
    let pulse = pulse_at(ui, index);
    let painter = ui.painter();
    let (fill, border, ink) = if on {
        (
            Color32::from_rgb(0x2A, 0x26, 0x1C),
            lerp_color(ACCENT_DIM, ACCENT, pulse),
            ACCENT,
        )
    } else {
        (CARD, STROKE, INK_3)
    };
    if on {
        // Soft glow halo behind the chip, breathing with the pulse.
        painter.rect_filled(
            rect.expand(2.0),
            CornerRadius::same(8),
            Color32::from_rgba_unmultiplied(ACCENT.r(), ACCENT.g(), ACCENT.b(), (26.0 * pulse) as u8),
        );
    }
    painter.rect(
        rect,
        CornerRadius::same(6),
        fill,
        Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center() - egui::vec2(0.0, 5.0),
        egui::Align2::CENTER_CENTER,
        index.to_string(),
        FontId::new(10.0, FontFamily::Monospace),
        ink,
    );
    // One dot per hardware thread, lit individually; lit dots carry the pulse.
    let n = threads.len().max(1) as f32;
    let dot = 5.0;
    let gap = 3.0;
    let total = n * dot + (n - 1.0) * gap;
    let mut x = rect.center().x - total / 2.0;
    for t in threads {
        let dot_rect =
            egui::Rect::from_min_size(egui::pos2(x, rect.bottom() - 11.0), egui::vec2(dot, dot));
        painter.rect_filled(
            dot_rect,
            CornerRadius::same(2),
            if lit.contains(t) {
                lerp_color(ACCENT_DIM, ACCENT, 0.4 + 0.6 * pulse)
            } else {
                STROKE
            },
        );
        x += dot + gap;
    }
}
