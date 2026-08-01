//! The one-click apply progress screen, drawn as a Black Desert enhancement
//! window. Every prop maps to something the run really does: the "material"
//! slot holds the optimization steps still to land, "protection" is the
//! config-file backups (a failure loses nothing), each landed step raises the
//! enhancement tier, and PEN is a fully applied setup. A failed step fails
//! the enhance; a clean run means the optimization succeeded.

use egui::{Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, pos2, vec2};
use egui_phosphor::regular as icons;

use crate::theme::ERR;

// BDO enhancement-window palette, used only on this screen.
const GOLD: Color32 = Color32::from_rgb(0xD8, 0xB4, 0x6A);
const GOLD_HI: Color32 = Color32::from_rgb(0xFF, 0xE9, 0xB0);
const GOLD_SOFT: Color32 = Color32::from_rgb(0xF0, 0xD0, 0x89);
const BRONZE: Color32 = Color32::from_rgb(0x8A, 0x6F, 0x3F);
const AMBER: Color32 = Color32::from_rgb(0xE8, 0xA3, 0x3D);
const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xD7, 0x6B);
const TEAL: Color32 = Color32::from_rgb(0x3E, 0xCF, 0x9A);
const DIM: Color32 = Color32::from_rgb(0x9B, 0x8A, 0x68);
const SLOT_BG: Color32 = Color32::from_rgb(0x1D, 0x17, 0x0C);
const COIN_BG: Color32 = Color32::from_rgb(0x14, 0x10, 0x08);

/// Enhancement tiers: one per landed step, PEN when everything applied.
const TIERS: [&str; 4] = ["PRI", "DUO", "TRI", "TET"];

const TAU: f32 = std::f32::consts::TAU;

/// Everything the strip needs from the running one-click state.
pub(crate) struct EnhanceView<'a> {
    /// Finished steps in order (the async driver step joins when it ends).
    pub steps: &'a [(String, Result<String, String>)],
    /// Set while the driver-profile worker is still running.
    pub pending_driver: Option<&'a str>,
    /// How many of `steps` the staggered reveal currently shows.
    pub visible: usize,
    /// Animated 0..1 progress.
    pub progress: f32,
    /// Seconds since the run started; times the spark bursts.
    pub elapsed: f32,
    /// 0 while running, then 0..1 over the completion finale.
    pub finale: f32,
    /// A revealed step failed, so the enhance fails.
    pub failed: bool,
    pub undoing: bool,
    /// What is landing right now, e.g. "Enhancing: Game config files".
    pub stage: String,
    /// Selected affinity mask, shown in the chance block.
    pub mask_hex: &'a str,
    pub mask_cores: usize,
}

impl EnhanceView<'_> {
    fn total(&self) -> usize {
        self.steps.len() + usize::from(self.pending_driver.is_some())
    }

    /// Steps that have both been revealed and succeeded.
    fn landed(&self) -> usize {
        self.steps
            .iter()
            .take(self.visible)
            .filter(|(_, r)| r.is_ok())
            .count()
    }

    fn tier(&self) -> String {
        let landed = self.landed();
        if landed == 0 {
            "+0".into()
        } else if landed >= self.total() && !self.failed {
            "PEN".into()
        } else {
            TIERS[(landed - 1).min(TIERS.len() - 1)].into()
        }
    }
}

fn alpha(c: Color32, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn display(size: f32) -> FontId {
    FontId::new(size, crate::theme::display())
}

fn mono(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Monospace)
}

fn body(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Proportional)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn diamond(p: &egui::Painter, c: Pos2, r: f32, fill: Color32, stroke: Stroke) {
    let pts = vec![
        pos2(c.x, c.y - r),
        pos2(c.x + r, c.y),
        pos2(c.x, c.y + r),
        pos2(c.x - r, c.y),
    ];
    p.add(Shape::convex_polygon(pts, fill, stroke));
}

fn arc(p: &egui::Painter, c: Pos2, r: f32, start: f32, sweep: f32, stroke: Stroke) {
    if sweep <= 0.0 {
        return;
    }
    let n = 64;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = start + sweep * i as f32 / n as f32;
            pos2(c.x + a.cos() * r, c.y + a.sin() * r)
        })
        .collect();
    p.add(Shape::line(pts, stroke));
}

fn dashed_circle(p: &egui::Painter, c: Pos2, r: f32, stroke: Stroke, dash: f32, gap: f32, phase: f32) {
    let step = dash + gap;
    let n = ((TAU * r) / step).max(4.0) as usize;
    let da = TAU / n as f32;
    let dash_a = da * dash / step;
    for k in 0..n {
        arc(p, c, r, phase + k as f32 * da, dash_a, stroke);
    }
}

/// A gold-bordered inventory slot with a label above and a quantity corner.
fn slot(
    p: &egui::Painter,
    center: Pos2,
    label: &str,
    qty: &str,
    icon: impl FnOnce(&egui::Painter, Pos2),
) {
    let rect = Rect::from_center_size(center, vec2(48.0, 48.0));
    p.text(
        pos2(center.x, rect.top() - 6.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        display(11.0),
        GOLD_SOFT,
    );
    p.rect(
        rect,
        egui::CornerRadius::ZERO,
        SLOT_BG,
        Stroke::new(1.5, GOLD),
        egui::StrokeKind::Inside,
    );
    p.rect(
        rect.shrink(3.0),
        egui::CornerRadius::ZERO,
        Color32::TRANSPARENT,
        Stroke::new(1.0, alpha(GOLD, 0.35)),
        egui::StrokeKind::Inside,
    );
    diamond(p, rect.right_top(), 5.0, GOLD, Stroke::NONE);
    icon(p, center);
    p.text(
        rect.right_bottom() + vec2(-3.0, -1.0),
        egui::Align2::RIGHT_BOTTOM,
        qty,
        mono(11.0),
        GOLD_HI,
    );
}

/// The "material" black-stone gem, faceted like the game item.
fn material_icon(p: &egui::Painter, c: Pos2) {
    let s = |dx: f32, dy: f32| pos2(c.x + dx, c.y + dy);
    p.add(Shape::convex_polygon(
        vec![s(0.0, -11.0), s(9.0, -4.0), s(5.5, 11.0), s(-5.5, 11.0), s(-9.0, -4.0)],
        Color32::from_rgb(0xCA, 0xA3, 0x38),
        Stroke::NONE,
    ));
    p.add(Shape::convex_polygon(
        vec![s(0.0, -11.0), s(9.0, -4.0), s(0.0, 0.0)],
        Color32::from_rgb(0xFF, 0xE0, 0x8A),
        Stroke::NONE,
    ));
    p.add(Shape::convex_polygon(
        vec![s(-9.0, -4.0), s(0.0, 0.0), s(-5.5, 11.0)],
        Color32::from_rgb(0x8F, 0x6F, 0x1F),
        Stroke::NONE,
    ));
}

/// The "protection" stone: teal orb, like a cron/valks item.
fn protection_icon(p: &egui::Painter, c: Pos2) {
    p.circle_filled(c, 10.0, TEAL);
    p.circle_filled(c + vec2(-3.0, -3.5), 4.0, Color32::from_rgb(0x8F, 0xF0, 0xDD));
    p.circle_stroke(c, 10.0, Stroke::new(1.5, Color32::from_rgb(0x1D, 0x7A, 0x6B)));
}

/// Short coin label for a real step name.
fn coin_label(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("affinity") {
        "AFFINITY".into()
    } else if n.contains("config") {
        "CONFIG".into()
    } else if n.contains("windowed") {
        "WINDOWED".into()
    } else if n.contains("nvidia") || n.contains("driver") {
        "DRIVER".into()
    } else {
        name.split_whitespace()
            .next()
            .unwrap_or(name)
            .to_uppercase()
    }
}

/// Draw the whole enhancement screen: strip, step coins, durability lines and
/// the current stage.
pub(crate) fn draw(ui: &mut egui::Ui, logo: &egui::TextureHandle, view: &EnhanceView) {
    let t = ui.input(|i| i.time) as f32;

    ui.add_space(6.0);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 285.0), Sense::hover());
    let center = pos2(rect.center().x, rect.top() + 142.0);
    let p = ui.painter();

    // Warm glow behind the core so the strip reads on the app's cool background.
    for (r, a) in [(240.0, 0.03), (170.0, 0.05), (120.0, 0.06)] {
        p.circle_filled(center, r, alpha(AMBER, a));
    }

    // ---------------------------------------------------------------- rail
    let rail_y = center.y;
    let core_half = 120.0;
    let segs = [
        (rect.left() + 30.0, center.x - core_half),
        (center.x + core_half, rect.right() - 30.0),
    ];
    for (x0, x1) in segs {
        p.line_segment(
            [pos2(x0, rail_y), pos2(x1, rail_y)],
            Stroke::new(2.0, alpha(BRONZE, 0.8)),
        );
    }
    // Traveling light on the rail.
    let travel = (t / 2.6).fract();
    let lx = rect.left() + 30.0 + travel * (rect.width() - 60.0);
    for (x0, x1) in segs {
        let a = (lx - 30.0).max(x0);
        let b = (lx + 30.0).min(x1);
        if a < b {
            p.line_segment(
                [pos2(a, rail_y), pos2(b, rail_y)],
                Stroke::new(2.5, alpha(GOLD_HI, 0.8)),
            );
        }
    }
    for frac in [0.26, 0.74] {
        let x = rect.left() + rect.width() * frac;
        diamond(
            p,
            pos2(x, rail_y),
            6.0,
            BRONZE,
            Stroke::new(1.0, GOLD),
        );
    }
    // Flame diamond at the far right of the rail.
    let flame_c = pos2(rect.right() - 30.0, rail_y);
    diamond(p, flame_c, 16.0, SLOT_BG, Stroke::new(1.5, GOLD));
    p.text(
        flame_c,
        egui::Align2::CENTER_CENTER,
        icons::FIRE,
        body(15.0),
        AMBER,
    );

    // ---------------------------------------------------------------- slots
    let remaining = view.total().saturating_sub(view.landed());
    slot(
        p,
        pos2(rect.left() + 74.0, rail_y),
        "MATERIAL",
        &remaining.to_string(),
        material_icon,
    );
    let prot_c = pos2(rect.left() + 158.0, rect.top() + 58.0);
    slot(p, prot_c, "PROTECTION", icons::INFINITY, protection_icon);
    // Vertical link from the protection slot down to the rail.
    p.line_segment(
        [pos2(prot_c.x, prot_c.y + 24.0), pos2(prot_c.x, rail_y)],
        Stroke::new(2.0, alpha(BRONZE, 0.8)),
    );

    // ---------------------------------------------------------------- core
    // Outer mandala: solid bronze ring, slowly rotating dashed gold ring and
    // four diamond studs.
    p.circle_stroke(center, 108.0, Stroke::new(1.0, BRONZE));
    dashed_circle(
        p,
        center,
        101.0,
        Stroke::new(1.5, alpha(GOLD, 0.8)),
        2.0,
        7.0,
        t * TAU / 40.0,
    );
    for k in 0..4 {
        let a = t * TAU / 40.0 + k as f32 * TAU / 4.0;
        diamond(
            p,
            center + vec2(a.cos(), a.sin()) * 108.0,
            5.0,
            GOLD,
            Stroke::NONE,
        );
    }
    // Inner mandala, counter-rotating.
    p.circle_stroke(center, 80.0, Stroke::new(1.0, alpha(BRONZE, 0.8)));
    dashed_circle(
        p,
        center,
        73.0,
        Stroke::new(1.0, alpha(GOLD, 0.55)),
        11.0,
        7.0,
        -t * TAU / 28.0,
    );
    for k in 0..4 {
        let a = -t * TAU / 28.0 + k as f32 * TAU / 4.0;
        p.circle_filled(
            center + vec2(a.cos(), a.sin()) * 80.0,
            3.0,
            alpha(GOLD_SOFT, 0.9),
        );
    }
    // Progress arc, glow underneath.
    let sweep = TAU * view.progress.clamp(0.0, 1.0);
    let start = -TAU / 4.0;
    arc(p, center, 108.0, start, sweep, Stroke::new(7.0, alpha(AMBER, 0.25)));
    arc(p, center, 108.0, start, sweep, Stroke::new(3.0, AMBER));

    // Emblem: glow cloud, rotated-diamond frame, gold square, the app mask.
    let pulse = 0.5 + 0.5 * (t * 2.6).sin();
    let glow = if view.finale > 0.0 && !view.failed { 1.0 } else { pulse };
    for (r, a) in [(66.0, 0.08), (50.0, 0.12), (40.0, 0.10)] {
        p.circle_filled(center, r, alpha(GOLD_HI, a * (0.7 + 0.6 * glow)));
    }
    let pop = if view.finale > 0.0 && !view.failed {
        1.0 + 0.20 * (std::f32::consts::PI * (view.finale / 0.45).min(1.0)).sin()
    } else {
        1.0
    };
    let half = 40.0 * pop;
    let diamond_pts: Vec<Pos2> = (0..4)
        .map(|k| {
            let a = k as f32 * TAU / 4.0;
            center + vec2(a.cos(), a.sin()) * (half + 13.0)
        })
        .collect();
    p.add(Shape::closed_line(
        diamond_pts,
        Stroke::new(1.0, alpha(GOLD, 0.4)),
    ));
    let emblem = Rect::from_center_size(center, vec2(half * 2.0, half * 2.0));
    p.rect(
        emblem,
        egui::CornerRadius::ZERO,
        Color32::from_rgb(0x21, 0x1A, 0x0E),
        Stroke::new(2.0, GOLD),
        egui::StrokeKind::Inside,
    );
    p.image(
        logo.id(),
        emblem.shrink(10.0 * pop),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    // Sparks when the newest step lands (red spray if that step failed).
    if view.visible > 0 && view.finale <= 0.0 {
        let i = view.visible - 1;
        let age = view.elapsed - i as f32 * 0.6;
        if (0.0..0.7).contains(&age) {
            let ok = view.steps.get(i).is_some_and(|(_, r)| r.is_ok());
            let fade = 1.0 - age / 0.7;
            for k in 0..12 {
                let ang = i as f32 * 1.7 + k as f32 * TAU / 12.0;
                let dist = ease_out(age / 0.7) * (46.0 + 6.5 * ((k * 13) % 5) as f32);
                let col = if ok {
                    [GOLD_HI, AMBER, YELLOW][k % 3]
                } else {
                    ERR
                };
                p.circle_filled(
                    center + vec2(ang.cos(), ang.sin()) * dist,
                    2.5,
                    alpha(col, fade),
                );
            }
        }
    }

    // Attempts pill, tier badge and percent — all real run numbers.
    let pill_text = format!("{} / {} STEPS", view.landed(), view.total());
    let galley = p.layout_no_wrap(pill_text, mono(12.0), crate::theme::INK);
    // Bottom-right of the core: the top corners are taken by the chance block
    // and the protection slot.
    let pill = Rect::from_center_size(
        pos2((center.x + 172.0).min(rect.right() - 90.0), center.y + 108.0),
        galley.size() + vec2(28.0, 14.0),
    );
    p.rect(
        pill,
        egui::CornerRadius::same(14),
        Color32::from_rgb(0x0C, 0x0A, 0x06),
        Stroke::new(1.5, BRONZE),
        egui::StrokeKind::Inside,
    );
    p.galley(pill.center() - galley.size() / 2.0, galley, crate::theme::INK);

    p.text(
        pos2(center.x, center.y - 66.0),
        egui::Align2::CENTER_BOTTOM,
        view.tier(),
        display(17.0),
        if view.failed { ERR } else { GOLD_HI },
    );
    p.text(
        pos2(center.x, center.y + 66.0),
        egui::Align2::CENTER_TOP,
        format!("{:.2}%", view.progress.clamp(0.0, 1.0) * 100.0),
        display(16.0),
        YELLOW,
    );

    // ---------------------------------------------------------- chance block
    let ch_x = rect.right() - 118.0;
    let ch_top = rect.top() + 34.0;
    p.text(
        pos2(ch_x, ch_top),
        egui::Align2::CENTER_TOP,
        if view.undoing { "EXTRACTION CHANCE" } else { "OPTIMIZATION CHANCE" },
        display(12.0),
        GOLD_SOFT,
    );
    p.text(
        pos2(ch_x, ch_top + 22.0),
        egui::Align2::CENTER_TOP,
        format!("{} {} CORES · 0x{}", icons::CPU, view.mask_cores, view.mask_hex.to_uppercase()),
        mono(11.5),
        crate::theme::INK,
    );
    let (big, big_col) = if view.failed {
        ("0.00%", ERR)
    } else {
        ("100.00%", AMBER)
    };
    p.text(
        pos2(ch_x, ch_top + 44.0),
        egui::Align2::CENTER_TOP,
        big,
        display(22.0),
        big_col,
    );
    p.text(
        pos2(ch_x, ch_top + 72.0),
        egui::Align2::CENTER_TOP,
        "deterministic — no RNG",
        body(10.0),
        alpha(YELLOW, 0.85),
    );

    // ---------------------------------------------------------------- finale
    if view.finale > 0.0 {
        let f = view.finale;
        let col = if view.failed { ERR } else { GOLD_HI };
        // Flash and expanding shockwave ring.
        p.circle_filled(center, 60.0 + f * 160.0, alpha(col, (1.0 - f) * 0.35));
        p.circle_stroke(
            center,
            20.0 + ease_out(f) * 240.0,
            Stroke::new(3.0, alpha(col, 1.0 - f)),
        );
        let msg = if view.failed {
            "ENHANCE FAILED — NOTHING LOST"
        } else if view.undoing {
            "EXTRACTION COMPLETE — SETUP RESTORED"
        } else {
            "SETUP ENHANCED — READY TO GRIND"
        };
        p.text(
            pos2(center.x, rect.top() + 12.0),
            egui::Align2::CENTER_TOP,
            msg,
            display(19.0),
            alpha(col, (f * 2.5).min(1.0)),
        );
    }

    // ------------------------------------------------------------ step coins
    let names: Vec<&str> = view
        .steps
        .iter()
        .map(|(n, _)| n.as_str())
        .chain(view.pending_driver)
        .collect();
    let n = names.len();
    if n > 0 {
        let (row, _) = ui.allocate_exact_size(vec2(width, 78.0), Sense::hover());
        let p = ui.painter();
        let coin_d = 46.0;
        let link = 22.0;
        let total_w = n as f32 * coin_d + (n as f32 - 1.0) * link;
        let mut x = row.center().x - total_w / 2.0 + coin_d / 2.0;
        let cy = row.top() + 28.0;
        for (i, name) in names.iter().enumerate() {
            let c = pos2(x, cy);
            if i > 0 {
                p.line_segment(
                    [pos2(x - coin_d / 2.0 - link, cy), pos2(x - coin_d / 2.0, cy)],
                    Stroke::new(2.0, alpha(BRONZE, 0.8)),
                );
            }
            let outcome = view.steps.get(i).map(|(_, r)| r.is_ok());
            let revealed = i < view.visible;
            let active = !revealed
                && (i == view.visible && i < view.steps.len()
                    || view.pending_driver.is_some()
                        && i == view.steps.len()
                        && view.visible == view.steps.len());
            let (ring, label_col) = match (revealed, outcome) {
                (true, Some(true)) => (GOLD, GOLD_SOFT),
                (true, Some(false)) => (ERR, ERR),
                _ if active => (GOLD_HI, GOLD_HI),
                _ => (alpha(BRONZE, 0.7), alpha(DIM, 0.6)),
            };
            let r = if active {
                coin_d / 2.0 + 1.5 * (t * 4.0).sin()
            } else {
                coin_d / 2.0
            };
            p.circle_filled(c, r, COIN_BG);
            p.circle_stroke(c, r, Stroke::new(1.5, ring));
            match (revealed, outcome) {
                (true, Some(true)) => {
                    p.text(c, egui::Align2::CENTER_CENTER, icons::CHECK, body(16.0), YELLOW);
                }
                (true, Some(false)) => {
                    p.text(c, egui::Align2::CENTER_CENTER, icons::X, body(16.0), ERR);
                }
                _ if active => {
                    p.text(
                        c,
                        egui::Align2::CENTER_CENTER,
                        (i + 1).to_string(),
                        display(15.0),
                        GOLD_HI,
                    );
                }
                _ => {
                    p.text(
                        c,
                        egui::Align2::CENTER_CENTER,
                        icons::LOCK_SIMPLE,
                        body(13.0),
                        alpha(BRONZE, 0.9),
                    );
                }
            }
            p.text(
                pos2(x, cy + coin_d / 2.0 + 8.0),
                egui::Align2::CENTER_TOP,
                coin_label(name),
                display(9.0),
                label_col,
            );
            x += coin_d + link;
        }
    }

    // ---------------------------------------------------- stage + durability
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(&view.stage).font(display(13.5)).color(AMBER));
        ui.add_space(4.0);
        let lines: [&str; 3] = if view.undoing {
            [
                "Restoring the original config files from their backups",
                "Windows setting put back exactly as it was",
                "Driver profile reset to its defaults",
            ]
        } else {
            [
                "Max Durability −0 upon failure — every config file is backed up first",
                "No Optimization Level decrease — a failed step never breaks what already landed",
                "No global Windows settings consumed — only BDO's shortcut, profile and files",
            ]
        };
        for (line, col) in lines
            .iter()
            .zip([AMBER, alpha(YELLOW, 0.9), alpha(GOLD_SOFT, 0.8)])
        {
            ui.label(RichText::new(*line).size(12.0).color(col));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(steps: &[(String, Result<String, String>)], visible: usize) -> EnhanceView<'_> {
        EnhanceView {
            steps,
            pending_driver: None,
            visible,
            progress: 0.0,
            elapsed: 0.0,
            finale: 0.0,
            failed: steps.iter().take(visible).any(|(_, r)| r.is_err()),
            undoing: false,
            stage: String::new(),
            mask_hex: "555",
            mask_cores: 6,
        }
    }

    #[test]
    fn tiers_climb_per_landed_step_and_cap_at_pen() {
        let ok = |n: &str| (n.to_string(), Ok(String::new()));
        let steps = vec![ok("a"), ok("b"), ok("c"), ok("d")];
        assert_eq!(view(&steps, 0).tier(), "+0");
        assert_eq!(view(&steps, 1).tier(), "PRI");
        assert_eq!(view(&steps, 3).tier(), "TRI");
        assert_eq!(view(&steps, 4).tier(), "PEN");

        // A failed step blocks PEN even with every step revealed.
        let mut with_fail = steps.clone();
        with_fail[3] = ("d".to_string(), Err("boom".to_string()));
        assert_eq!(view(&with_fail, 4).tier(), "TRI");
    }

    /// The whole screen is hand-painted, so a bad index or an unregistered font
    /// family only shows up at paint time. Run the real `draw` headlessly over
    /// every stage a run passes through — empty, mid-run, driver still pending,
    /// a failed step, and both finales — and let a panic fail the test.
    #[test]
    fn draw_survives_every_stage_of_a_run() {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let logo = ctx.load_texture(
            "test-logo",
            egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]),
            Default::default(),
        );

        let ok = |n: &str| (n.to_string(), Ok("done".to_string()));
        let full = vec![
            ok("CPU affinity"),
            ok("Game config files"),
            ok("Optimizations for windowed games"),
            ok("NVIDIA driver profile"),
        ];
        let mut failed_run = full.clone();
        failed_run[1] = ("Game config files".into(), Err("locked".into()));

        let paint = |steps: &[(String, Result<String, String>)],
                         pending: Option<&str>,
                         visible: usize,
                         progress: f32,
                         finale: f32,
                         undoing: bool| {
            let view = EnhanceView {
                steps,
                pending_driver: pending,
                visible,
                progress,
                elapsed: visible as f32 * 0.6,
                finale,
                failed: steps.iter().take(visible).any(|(_, r)| r.is_err()),
                undoing,
                stage: "Enhancing: something".to_string(),
                mask_hex: "5554",
                mask_cores: 7,
            };
            let _ = ctx.run_ui(Default::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| draw(ui, &logo, &view));
            });
        };

        let driver = Some("NVIDIA driver profile");
        paint(&[], None, 0, 0.0, 0.0, false); // nothing has run yet
        paint(&full[..3], driver, 0, 0.0, 0.0, false); // driver still pending
        paint(&full[..3], driver, 3, 0.75, 0.0, false); // waiting on the driver
        paint(&full, None, 2, 0.5, 0.0, false); // mid reveal
        paint(&failed_run, None, 4, 1.0, 0.0, false); // a step failed
        paint(&failed_run, None, 4, 1.0, 0.6, false); // failed finale
        paint(&full, None, 4, 1.0, 1.0, true); // undo finale
    }
}
