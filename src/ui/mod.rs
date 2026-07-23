mod fonts;
mod theme;

use crate::clients::ClientEntry;
use crate::history::HistoryItem;
use crate::net::peer::PeerSnapshot;
use eframe::egui;
use theme::GlassTheme;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    History,
    Devices,
    Settings,
}

#[derive(Clone)]
pub struct NearbyDevice {
    pub name: String,
    pub device_id: String,
    pub addr: String,
}

#[derive(Clone)]
pub struct UiState {
    tab: Tab,
    pub device_name: String,
    pub password: String,
    /// When true, shared password is shown in plain text.
    pub show_password: bool,
    pub tcp_port: String,
    pub udp_port: String,
    pub max_payload_mb: String,
    pub sync_enabled: bool,
    /// Start with tray only (no main window). Persisted as config.start_minimized_to_tray.
    pub start_minimized_to_tray: bool,
    /// Launch at user login. Persisted as config.auto_start + OS startup entry.
    pub auto_start: bool,
    /// Active UI language code (`en_us` / `zh_cn`). Hot-reloaded; persisted immediately.
    pub language: String,
    /// Set when user changes language combo — runtime persists config.language at once.
    pub cmd_set_language: Option<String>,
    pub manual_addr: String,
    pub search: String,
    pub status_line: String,
    pub firewall_hint: Option<String>,
    pub peers: Vec<PeerSnapshot>,
    pub nearby: Vec<NearbyDevice>,
    /// From clients.json (discovered + saved).
    pub saved_clients: Vec<ClientEntry>,
    pub history: Vec<HistoryItem>,
    pub toast: Option<String>,
    /// Frames left to show toast (cleared automatically).
    pub toast_ttl_frames: u32,
    theme: GlassTheme,
    pub cmd_save_settings: bool,
    /// Open ~/.ohmycopy in the system file manager.
    pub cmd_open_config_folder: bool,
    pub cmd_add_manual: bool,
    /// Connect discovered (trial): (device_id, name, addr) — clients only after auth OK.
    pub cmd_connect_nearby: Option<(String, String, String)>,
    /// Remove from clients.json: (device_id?, addr) — after user confirms.
    pub cmd_remove_client: Option<(Option<Uuid>, String)>,
    /// Pending remove confirmation dialog: (device_id?, addr, name)
    pub confirm_remove: Option<(Option<Uuid>, String, String)>,
    /// Toggle ignore: (device_id?, addr, ignored)
    pub cmd_set_ignore: Option<(Option<Uuid>, String, bool)>,
    pub cmd_reload_clients: bool,
    pub cmd_clear_history: bool,
    pub cmd_copy_history: Option<String>,
    pub cmd_toggle_sync: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tab: Tab::History,
            device_name: String::new(),
            password: String::new(),
            show_password: false,
            tcp_port: "3721".into(),
            udp_port: "3721".into(),
            max_payload_mb: "10".into(),
            sync_enabled: true,
            start_minimized_to_tray: false,
            auto_start: false,
            language: crate::i18n::LANG_EN.to_string(),
            cmd_set_language: None,
            manual_addr: String::new(),
            search: String::new(),
            status_line: crate::i18n::t("app.starting"),
            firewall_hint: None,
            peers: Vec::new(),
            nearby: Vec::new(),
            saved_clients: Vec::new(),
            history: Vec::new(),
            toast: None,
            toast_ttl_frames: 0,
            theme: GlassTheme::dark(),
            cmd_save_settings: false,
            cmd_open_config_folder: false,
            cmd_add_manual: false,
            cmd_connect_nearby: None,
            cmd_remove_client: None,
            confirm_remove: None,
            cmd_set_ignore: None,
            cmd_reload_clients: false,
            cmd_clear_history: false,
            cmd_copy_history: None,
            cmd_toggle_sync: false,
        }
    }
}

pub struct OhMyCopyApp {
    pub ui: UiState,
}

impl OhMyCopyApp {
    pub fn new(cc: &eframe::CreationContext<'_>, ui: UiState) -> Self {
        fonts::install_cjk_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &GlassTheme::dark());
        Self { ui }
    }
}

impl eframe::App for OhMyCopyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let t = self.ui.theme.clone();

        // Toast auto-expire (~3s at 200ms repaint)
        if self.ui.toast_ttl_frames > 0 {
            self.ui.toast_ttl_frames = self.ui.toast_ttl_frames.saturating_sub(1);
            if self.ui.toast_ttl_frames == 0 {
                self.ui.toast = None;
            }
        }

        // Slightly wider horizontal margin so edge buttons/checkboxes are not
        // clipped by the window / panel clip by ~1–2px (stroke + AA).
        let bar_frame = egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 4))
            .fill(t.bg);
        let central_frame = egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 8))
            .fill(t.bg);

        egui::TopBottomPanel::top("top")
            .frame(bar_frame)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading(egui::RichText::new("OhMyCopy").color(t.accent).strong());
                    ui.label(
                        egui::RichText::new(crate::i18n::t("app.subtitle"))
                            .color(t.text_muted)
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let sync_label = if self.ui.sync_enabled {
                            crate::i18n::t("app.sync_on")
                        } else {
                            crate::i18n::t("app.sync_off")
                        };
                        if ui.add(egui::Button::new(sync_label).fill(t.card)).clicked() {
                            self.ui.sync_enabled = !self.ui.sync_enabled;
                            self.ui.cmd_toggle_sync = true;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    tab_btn(
                        ui,
                        &mut self.ui.tab,
                        Tab::History,
                        crate::i18n::t("tab.history"),
                        &t,
                    );
                    tab_btn(
                        ui,
                        &mut self.ui.tab,
                        Tab::Devices,
                        crate::i18n::t("tab.devices"),
                        &t,
                    );
                    tab_btn(
                        ui,
                        &mut self.ui.tab,
                        Tab::Settings,
                        crate::i18n::t("tab.settings"),
                        &t,
                    );
                });
                ui.add_space(4.0);
            });

        // Fixed height avoids toast show/hide resizing the bar (visual flicker).
        let bottom_h = if self.ui.firewall_hint.is_some() {
            56.0
        } else {
            32.0
        };
        egui::TopBottomPanel::bottom("bottom")
            .frame(bar_frame)
            .exact_height(bottom_h)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                if let Some(hint) = &self.ui.firewall_hint {
                    ui.colored_label(t.warning, hint);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&self.ui.status_line)
                            .color(t.text_muted)
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(toast) = &self.ui.toast {
                            ui.colored_label(t.accent, toast);
                        } else {
                            ui.label(egui::RichText::new(" ").small());
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ctx, |ui| match self.ui.tab {
                Tab::History => draw_history(ui, &mut self.ui, &t),
                Tab::Devices => draw_devices(ui, &mut self.ui, &t),
                Tab::Settings => draw_settings(ui, &mut self.ui, &t),
            });

        // Only repaint frequently while a toast is counting down.
        if self.ui.toast_ttl_frames > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(1000));
        }
    }
}

fn tab_btn(
    ui: &mut egui::Ui,
    current: &mut Tab,
    tab: Tab,
    label: impl Into<String>,
    t: &GlassTheme,
) {
    let label = label.into();
    let selected = *current == tab;
    let fill = if selected {
        t.accent.gamma_multiply(0.35)
    } else {
        t.card
    };
    if ui
        .add(egui::Button::new(egui::RichText::new(label).color(t.text)).fill(fill))
        .clicked()
    {
        *current = tab;
    }
}

/// Full-bleed card that stretches to the entire central panel (same look on every tab).
fn glass_panel(ui: &mut egui::Ui, t: &GlassTheme, add_contents: impl FnOnce(&mut egui::Ui)) {
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());

    ui.painter().rect(
        rect,
        egui::CornerRadius::same(10),
        t.card,
        egui::Stroke::new(1.0_f32, t.border),
        egui::StrokeKind::Inside,
    );

    // Inset content from the card edge. Keep a little room so checkbox/button
    // strokes and AA fringes on the far left/right are not clipped by 1–2px.
    let pad = 16.0;
    let inner = rect.shrink(pad);
    // Layout to `inner`, but clip a couple of pixels wider so edge widgets
    // still paint fully while staying inside the card border.
    let clip = inner.expand(2.0).intersect(rect.shrink(1.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.set_clip_rect(clip.intersect(ui.clip_rect()));
        ui.set_min_size(inner.size());
        add_contents(ui);
    });
}

fn format_time(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64) {
        chrono::LocalResult::Single(dt) => dt.format("%m-%d %H:%M:%S").to_string(),
        _ => ms.to_string(),
    }
}

/// Collapse whitespace and hard-limit length.
fn display_preview(s: &str, max_chars: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let n = flat.chars().count();
    if n <= max_chars {
        flat
    } else {
        format!(
            "{}…",
            flat.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

/// One history row with a fixed bounding box — cannot grow past `full_w`.
/// Returns true if the user clicked 复制.
fn history_row(ui: &mut egui::Ui, full_w: f32, preview: &str, meta: &str, t: &GlassTheme) -> bool {
    const ROW_H: f32 = 52.0;
    const BTN_W: f32 = 52.0;
    const BTN_H: f32 = 28.0;
    const PAD: f32 = 10.0;
    const GAP: f32 = 8.0;

    let (row_rect, _resp) = ui.allocate_exact_size(egui::vec2(full_w, ROW_H), egui::Sense::hover());

    // Background card
    let card = row_rect.shrink2(egui::vec2(0.0, 2.0));
    ui.painter().rect(
        card,
        egui::CornerRadius::same(8),
        t.card.gamma_multiply(0.55),
        egui::Stroke::new(1.0_f32, t.border),
        egui::StrokeKind::Inside,
    );

    // Button rect (right side, fixed)
    let btn_rect = egui::Rect::from_center_size(
        egui::pos2(card.right() - PAD - BTN_W * 0.5, card.center().y),
        egui::vec2(BTN_W, BTN_H),
    );

    // Text rect (left, everything left of the button)
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(card.left() + PAD, card.top() + 6.0),
        egui::pos2(btn_rect.left() - GAP, card.bottom() - 6.0),
    );

    // Character budget from pixel width (~11px average for mixed CJK/latin)
    let max_chars = ((text_rect.width() / 11.0) as usize).clamp(12, 72);
    let line = display_preview(preview, max_chars);

    // Paint text inside clipped child ui so nothing can spill past text_rect.
    let mut clicked = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
        ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.vertical(|ui| {
            ui.set_max_width(text_rect.width());
            ui.add(egui::Label::new(egui::RichText::new(&line).color(t.text)).truncate());
            ui.add(
                egui::Label::new(egui::RichText::new(meta).small().color(t.text_muted)).truncate(),
            );
        });
    });

    if ui
        .put(
            btn_rect,
            egui::Button::new(crate::i18n::t("history.copy")).fill(t.accent.gamma_multiply(0.35)),
        )
        .clicked()
    {
        clicked = true;
    }
    clicked
}

fn draw_history(ui: &mut egui::Ui, state: &mut UiState, t: &GlassTheme) {
    glass_panel(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.label(crate::i18n::t("history.search"));
            ui.add(
                egui::TextEdit::singleline(&mut state.search)
                    .desired_width(200.0)
                    .hint_text(crate::i18n::t("history.filter_hint")),
            );
            if ui.button(crate::i18n::t("history.clear")).clicked() {
                state.cmd_clear_history = true;
            }
        });
        ui.add_space(8.0);

        let list_w = ui.available_width();

        egui::ScrollArea::vertical()
            .id_salt("history_scroll")
            .max_width(list_w)
            .auto_shrink([false, false])
            .hscroll(false)
            .show(ui, |ui| {
                ui.set_max_width(list_w);

                if state.history.is_empty() {
                    ui.label(
                        egui::RichText::new(crate::i18n::t("history.empty")).color(t.text_muted),
                    );
                    return;
                }

                let mut copy_cmd: Option<String> = None;
                let q = state.search.to_lowercase();
                let row_w = (list_w - 4.0).max(120.0);

                for item in &state.history {
                    if !q.is_empty()
                        && !item.preview.to_lowercase().contains(&q)
                        && !item.content.to_lowercase().contains(&q)
                    {
                        continue;
                    }

                    let meta = format!("{} · {}", item.kind, format_time(item.created_at));
                    if history_row(ui, row_w, item.preview.trim(), &meta, t) {
                        copy_cmd = Some(item.content.clone());
                    }
                }

                if let Some(text) = copy_cmd {
                    state.cmd_copy_history = Some(text);
                }
            });
    });
}

fn draw_devices(ui: &mut egui::Ui, state: &mut UiState, t: &GlassTheme) {
    glass_panel(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(crate::i18n::t("devices.mine"))
                    .strong()
                    .color(t.text),
            );
            if ui.button(crate::i18n::t("devices.refresh")).clicked() {
                state.cmd_reload_clients = true;
            }
        });
        ui.label(
            egui::RichText::new(crate::i18n::t("devices.mine_help"))
                .small()
                .color(t.text_muted),
        );
        ui.add_space(6.0);

        let mut set_ignore: Option<(Option<Uuid>, String, bool)> = None;
        let mut open_remove_confirm: Option<(Option<Uuid>, String, String)> = None;

        egui::ScrollArea::vertical()
            .id_salt("clients_scroll")
            .max_height(180.0)
            .show(ui, |ui| {
                if state.saved_clients.is_empty() {
                    ui.label(
                        egui::RichText::new(crate::i18n::t("devices.none")).color(t.text_muted),
                    );
                }
                for c in &mut state.saved_clients {
                    let already = state.peers.iter().find(|p| {
                        (c.device_id.is_some() && Some(p.device_id) == c.device_id)
                            || p.addr == c.addr
                    });
                    let connected = already.map(|p| p.connected).unwrap_or(false);
                    let connecting = already.map(|p| p.connecting).unwrap_or(false);
                    let status = if c.ignored {
                        crate::i18n::t("devices.paused")
                    } else {
                        already
                            .map(|p| p.status.clone())
                            .unwrap_or_else(|| crate::i18n::t("devices.waiting"))
                    };

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&c.name).color(t.text));
                            ui.label(
                                egui::RichText::new(format!("{} · {}", c.addr, status))
                                    .small()
                                    .color(t.text_muted),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(crate::i18n::t("devices.remove"))
                                        .fill(egui::Color32::from_rgb(90, 40, 40)),
                                )
                                .on_hover_text(crate::i18n::t("devices.remove_hint"))
                                .clicked()
                            {
                                open_remove_confirm =
                                    Some((c.device_id, c.addr.clone(), c.name.clone()));
                            }
                            // Simple toggle: text only changes; slight accent when on.
                            let ign_label = if c.ignored {
                                crate::i18n::t("devices.resume_sync")
                            } else {
                                crate::i18n::t("devices.pause_sync")
                            };
                            let ign_btn = if c.ignored {
                                egui::Button::new(egui::RichText::new(ign_label).color(t.warning))
                                    .fill(egui::Color32::from_rgb(55, 48, 32))
                            } else {
                                egui::Button::new(
                                    egui::RichText::new(ign_label).color(t.text_muted),
                                )
                                .fill(t.card)
                            };
                            if ui
                                .add(ign_btn)
                                .on_hover_text(if c.ignored {
                                    crate::i18n::t("devices.resume_hint")
                                } else {
                                    crate::i18n::t("devices.pause_hint")
                                })
                                .clicked()
                            {
                                let new_val = !c.ignored;
                                c.ignored = new_val; // optimistic UI flip
                                set_ignore = Some((c.device_id, c.addr.clone(), new_val));
                            }
                            if connected {
                                ui.label(
                                    egui::RichText::new(crate::i18n::t("devices.connected"))
                                        .color(t.accent)
                                        .small(),
                                );
                            } else if connecting {
                                ui.label(
                                    egui::RichText::new(crate::i18n::t("devices.connecting"))
                                        .color(t.warning)
                                        .small(),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(crate::i18n::t("devices.auto_reconnect"))
                                        .color(t.text_muted)
                                        .small(),
                                );
                            }
                        });
                    });
                    ui.separator();
                }
            });

        if let Some(cmd) = set_ignore {
            state.cmd_set_ignore = Some(cmd);
        }
        if let Some(c) = open_remove_confirm {
            state.confirm_remove = Some(c);
        }

        // Secondary confirmation for remove.
        if let Some((id, addr, name)) = state.confirm_remove.clone() {
            let mut close = false;
            let mut confirmed = false;
            egui::Window::new(crate::i18n::t("devices.confirm_remove_title"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(320.0);
                    ui.label(
                        egui::RichText::new(crate::i18n::t_args(
                            "devices.confirm_remove_body",
                            &[("name", &name)],
                        ))
                        .strong()
                        .color(t.text),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(crate::i18n::t_args(
                            "devices.confirm_remove_detail",
                            &[("addr", &addr)],
                        ))
                        .small()
                        .color(t.text_muted),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(crate::i18n::t("devices.cancel"))
                                    .min_size(egui::vec2(80.0, 28.0)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(crate::i18n::t("devices.confirm_remove_btn"))
                                    .fill(egui::Color32::from_rgb(140, 40, 40))
                                    .min_size(egui::vec2(100.0, 28.0)),
                            )
                            .clicked()
                        {
                            confirmed = true;
                            close = true;
                        }
                    });
                });
            if confirmed {
                state.cmd_remove_client = Some((id, addr));
            }
            if close {
                state.confirm_remove = None;
            }
        }

        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(crate::i18n::t("devices.nearby"))
                .strong()
                .color(t.text),
        );
        ui.label(
            egui::RichText::new(crate::i18n::t("devices.nearby_help"))
                .small()
                .color(t.text_muted),
        );
        ui.add_space(6.0);

        let mut connect_nearby: Option<(String, String, String)> = None;

        // Nearby already filtered in backend to exclude saved clients.
        if state.nearby.is_empty() {
            ui.label(
                egui::RichText::new(crate::i18n::t("devices.nearby_none"))
                    .color(t.text_muted)
                    .small(),
            );
        } else {
            egui::ScrollArea::vertical()
                .id_salt("nearby_scroll")
                .max_height(140.0)
                .show(ui, |ui| {
                    for d in &state.nearby {
                        let already = state
                            .peers
                            .iter()
                            .find(|p| p.device_id.to_string() == d.device_id || p.addr == d.addr);
                        let connecting = already.map(|p| p.connecting).unwrap_or(false);
                        let auth_failed = already
                            .map(|p| p.status_kind == crate::net::peer::PeerStatus::AuthFailed)
                            .unwrap_or(false);

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&d.name).color(t.text));
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        short_id(&d.device_id),
                                        d.addr
                                    ))
                                    .small()
                                    .color(t.text_muted),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if connecting {
                                        ui.label(
                                            egui::RichText::new(crate::i18n::t(
                                                "devices.connecting",
                                            ))
                                            .color(t.warning)
                                            .small(),
                                        );
                                    } else if ui
                                        .add(
                                            egui::Button::new(crate::i18n::t("devices.connect"))
                                                .fill(t.accent.gamma_multiply(0.45)),
                                        )
                                        .clicked()
                                    {
                                        connect_nearby = Some((
                                            d.device_id.clone(),
                                            d.name.clone(),
                                            d.addr.clone(),
                                        ));
                                    }
                                    if auth_failed {
                                        ui.label(
                                            egui::RichText::new(crate::i18n::t(
                                                "devices.bad_password",
                                            ))
                                            .color(t.warning)
                                            .small(),
                                        );
                                    }
                                },
                            );
                        });
                        ui.separator();
                    }
                });
        }

        if let Some(cmd) = connect_nearby {
            state.cmd_connect_nearby = Some(cmd);
        }

        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(crate::i18n::t("devices.manual"))
                .strong()
                .color(t.text),
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.manual_addr)
                    .desired_width(200.0)
                    .hint_text(crate::i18n::t("devices.manual_hint")),
            );
            if ui
                .add(
                    egui::Button::new(crate::i18n::t("devices.connect"))
                        .fill(t.accent.gamma_multiply(0.45)),
                )
                .clicked()
            {
                state.cmd_add_manual = true;
            }
        });
        ui.label(
            egui::RichText::new(crate::i18n::t("devices.manual_help"))
                .small()
                .color(t.text_muted),
        );
    });
}

fn short_id(id: &str) -> String {
    if id.len() > 8 {
        format!("{}…", &id[..8])
    } else {
        id.to_string()
    }
}

fn draw_settings(ui: &mut egui::Ui, state: &mut UiState, t: &GlassTheme) {
    glass_panel(ui, t, |ui| {
        // Scroll so action buttons stay reachable when the window is short
        // or the bottom status/firewall bar steals height.
        let list_w = ui.available_width();
        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .max_width(list_w)
            .auto_shrink([false, false])
            .hscroll(false)
            .show(ui, |ui| {
                ui.set_max_width(list_w);
                // Stretch content column like history/devices cards.
                let col_w = ui.available_width().min(520.0);

                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.basic"))
                        .strong()
                        .color(t.text),
                );
                ui.add_space(8.0);

                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([16.0, 12.0])
                    .min_col_width(110.0)
                    .show(ui, |ui| {
                        let field_w = (col_w - 130.0).clamp(180.0, 360.0);

                        // Language: hot-reload + immediate config write (not behind Save).
                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.language")).color(t.text),
                        );
                        let langs = crate::i18n::available_languages();
                        let current_label = langs
                            .iter()
                            .find(|(c, _)| c == &state.language)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| state.language.clone());
                        egui::ComboBox::from_id_salt("settings_language")
                            .width(field_w)
                            .selected_text(current_label)
                            .show_ui(ui, |ui| {
                                for (code, name) in &langs {
                                    if ui.selectable_label(state.language == *code, name).clicked()
                                        && state.language != *code
                                    {
                                        state.language = code.clone();
                                        let _ = crate::i18n::set_language(code);
                                        state.cmd_set_language = Some(code.clone());
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.device_name"))
                                .color(t.text),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.device_name)
                                .desired_width(field_w),
                        );
                        ui.end_row();

                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.password")).color(t.text),
                        );
                        ui.horizontal(|ui| {
                            let eye_w = 40.0;
                            let edit_w = (field_w - eye_w - 6.0).max(120.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut state.password)
                                    .password(!state.show_password)
                                    .desired_width(edit_w),
                            );
                            // Toggle visibility (text works reliably with CJK system fonts)
                            let eye_label = if state.show_password {
                                crate::i18n::t("settings.hide")
                            } else {
                                crate::i18n::t("settings.show")
                            };
                            if ui
                                .add_sized(
                                    [eye_w, 24.0],
                                    egui::Button::new(
                                        egui::RichText::new(eye_label).small().color(t.text_muted),
                                    )
                                    .fill(t.card),
                                )
                                .on_hover_text(if state.show_password {
                                    crate::i18n::t("settings.hide_password")
                                } else {
                                    crate::i18n::t("settings.show_password")
                                })
                                .clicked()
                            {
                                state.show_password = !state.show_password;
                            }
                        });
                        ui.end_row();

                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.tcp_port")).color(t.text),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.tcp_port).desired_width(field_w),
                        );
                        ui.end_row();

                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.udp_port")).color(t.text),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut state.udp_port).desired_width(field_w),
                        );
                        ui.end_row();

                        ui.label(
                            egui::RichText::new(crate::i18n::t("settings.max_payload"))
                                .color(t.text),
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.max_payload_mb)
                                    .desired_width((field_w - 40.0).max(80.0)),
                            );
                            ui.label(
                                egui::RichText::new(crate::i18n::t("settings.mb"))
                                    .color(t.text_muted),
                            );
                        });
                        ui.end_row();
                    });
                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.language_help"))
                        .small()
                        .color(t.text_muted),
                );
                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.max_payload_help"))
                        .small()
                        .color(t.text_muted),
                );

                if crate::config::Config::is_insecure_default_password(&state.password) {
                    ui.add_space(10.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 80, 80),
                        crate::i18n::t("settings.insecure_password"),
                    );
                }

                ui.add_space(12.0);
                ui.checkbox(
                    &mut state.auto_start,
                    egui::RichText::new(crate::i18n::t("settings.auto_start")).color(t.text),
                );
                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.auto_start_help"))
                        .small()
                        .color(t.text_muted),
                );

                ui.add_space(8.0);
                ui.checkbox(
                    &mut state.start_minimized_to_tray,
                    egui::RichText::new(crate::i18n::t("settings.start_tray")).color(t.text),
                );
                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.start_tray_help"))
                        .small()
                        .color(t.text_muted),
                );

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(crate::i18n::t("settings.save"))
                                .fill(t.accent.gamma_multiply(0.5))
                                .min_size(egui::vec2(120.0, 32.0)),
                        )
                        .clicked()
                    {
                        state.cmd_save_settings = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(crate::i18n::t("settings.open_data"))
                                .fill(t.card)
                                .min_size(egui::vec2(140.0, 32.0)),
                        )
                        .on_hover_text(crate::i18n::t("settings.open_data_hint"))
                        .clicked()
                    {
                        state.cmd_open_config_folder = true;
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(crate::i18n::t("settings.port_password_hint"))
                        .small()
                        .color(t.warning),
                );
                ui.add_space(4.0);
                let cfg_hint = crate::config::Config::config_dir()
                    .map(|p| {
                        crate::i18n::t_args(
                            "settings.data_path",
                            &[("path", &p.display().to_string())],
                        )
                    })
                    .unwrap_or_else(|_| crate::i18n::t("settings.data_path_fallback"));
                ui.label(egui::RichText::new(cfg_hint).small().color(t.text_muted));
                // Bottom breathing room so last controls aren't flush against the clip.
                ui.add_space(8.0);
            });
    });
}
