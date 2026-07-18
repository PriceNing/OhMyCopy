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
            manual_addr: String::new(),
            search: String::new(),
            status_line: "启动中…".into(),
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

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("OhMyCopy").color(t.accent).strong());
                ui.label(
                    egui::RichText::new("局域网剪贴板同步")
                        .color(t.text_muted)
                        .small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let sync_label = if self.ui.sync_enabled {
                        "同步：开"
                    } else {
                        "同步：关"
                    };
                    if ui
                        .add(egui::Button::new(sync_label).fill(t.card))
                        .clicked()
                    {
                        self.ui.sync_enabled = !self.ui.sync_enabled;
                        self.ui.cmd_toggle_sync = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                tab_btn(ui, &mut self.ui.tab, Tab::History, "历史", &t);
                tab_btn(ui, &mut self.ui.tab, Tab::Devices, "设备", &t);
                tab_btn(ui, &mut self.ui.tab, Tab::Settings, "设置", &t);
            });
            ui.add_space(4.0);
        });

        // Fixed height avoids toast show/hide resizing the bar (visual flicker).
        let bottom_h = if self.ui.firewall_hint.is_some() {
            52.0
        } else {
            28.0
        };
        egui::TopBottomPanel::bottom("bottom")
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

        egui::CentralPanel::default().show(ctx, |ui| match self.ui.tab {
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

fn tab_btn(ui: &mut egui::Ui, current: &mut Tab, tab: Tab, label: &str, t: &GlassTheme) {
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
        egui::Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink(12.0);
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.set_clip_rect(inner.intersect(ui.clip_rect()));
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
            flat.chars().take(max_chars.saturating_sub(1)).collect::<String>()
        )
    }
}

/// One history row with a fixed bounding box — cannot grow past `full_w`.
/// Returns true if the user clicked 复制.
fn history_row(
    ui: &mut egui::Ui,
    full_w: f32,
    preview: &str,
    meta: &str,
    t: &GlassTheme,
) -> bool {
    const ROW_H: f32 = 52.0;
    const BTN_W: f32 = 52.0;
    const BTN_H: f32 = 28.0;
    const PAD: f32 = 10.0;
    const GAP: f32 = 8.0;

    let (row_rect, _resp) =
        ui.allocate_exact_size(egui::vec2(full_w, ROW_H), egui::Sense::hover());

    // Background card
    let card = row_rect.shrink2(egui::vec2(0.0, 2.0));
    ui.painter().rect(
        card,
        egui::CornerRadius::same(8),
        t.card.gamma_multiply(0.55),
        egui::Stroke::new(1.0, t.border),
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
            ui.add(
                egui::Label::new(egui::RichText::new(&line).color(t.text)).truncate(),
            );
            ui.add(
                egui::Label::new(egui::RichText::new(meta).small().color(t.text_muted))
                    .truncate(),
            );
        });
    });

    if ui.put(btn_rect, egui::Button::new("复制").fill(t.accent.gamma_multiply(0.35)))
        .clicked()
    {
        clicked = true;
    }
    clicked
}

fn draw_history(ui: &mut egui::Ui, state: &mut UiState, t: &GlassTheme) {
    glass_panel(ui, t, |ui| {
        ui.horizontal(|ui| {
            ui.label("搜索");
            ui.add(
                egui::TextEdit::singleline(&mut state.search)
                    .desired_width(200.0)
                    .hint_text("过滤历史…"),
            );
            if ui.button("清空历史").clicked() {
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
                        egui::RichText::new(
                            "暂无历史。复制文本、图片/截图或文件后将出现在这里。",
                        )
                        .color(t.text_muted),
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
                egui::RichText::new("已添加客户端（自动连接）")
                    .strong()
                    .color(t.text),
            );
            if ui.button("重新加载").clicked() {
                state.cmd_reload_clients = true;
            }
        });
        ui.label(
            egui::RichText::new(
                "配对成功后两端都会出现在对方的客户端列表并自动重连。「忽略」仅暂停剪贴板；「移除」会通知对端双方都删除。",
            )
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
                        egui::RichText::new(
                            "暂无客户端。在「附近设备」点连接（密码正确）或手动添加后会出现在这里。",
                        )
                        .color(t.text_muted),
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
                        "已忽略 · 不同步"
                    } else {
                        already
                            .map(|p| p.status.as_str())
                            .unwrap_or("等待连接…")
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
                                    egui::Button::new("移除")
                                        .fill(egui::Color32::from_rgb(90, 40, 40)),
                                )
                                .on_hover_text("从列表移除并断开（需二次确认）")
                                .clicked()
                            {
                                open_remove_confirm =
                                    Some((c.device_id, c.addr.clone(), c.name.clone()));
                            }
                            // Simple toggle: text only changes; slight accent when on.
                            let ign_label = if c.ignored { "取消忽略" } else { "忽略" };
                            let ign_btn = if c.ignored {
                                egui::Button::new(
                                    egui::RichText::new(ign_label).color(t.warning),
                                )
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
                                    "恢复与该设备的双向剪贴板同步"
                                } else {
                                    "暂停与该设备的双向剪贴板同步（连接保留）"
                                })
                                .clicked()
                            {
                                let new_val = !c.ignored;
                                c.ignored = new_val; // optimistic UI flip
                                set_ignore =
                                    Some((c.device_id, c.addr.clone(), new_val));
                            }
                            if connected {
                                ui.label(egui::RichText::new("已连接").color(t.accent).small());
                            } else if connecting {
                                ui.label(
                                    egui::RichText::new("连接中…").color(t.warning).small(),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("自动重连中")
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
            egui::Window::new("确认移除客户端")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(320.0);
                    ui.label(
                        egui::RichText::new(format!("确定移除「{name}」吗？"))
                            .strong()
                            .color(t.text),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{addr}\n移除后将断开连接并停止自动同步，设备会回到附近发现列表。"
                        ))
                        .small()
                        .color(t.text_muted),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new("取消")
                                    .min_size(egui::vec2(80.0, 28.0)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new("确认移除")
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
            egui::RichText::new("附近设备（UDP 发现）")
                .strong()
                .color(t.text),
        );
        ui.label(
            egui::RichText::new(
                "点「连接」校验密码：成功则双方互相加入客户端列表；失败会提示且不加入。",
            )
            .small()
            .color(t.text_muted),
        );
        ui.add_space(6.0);

        let mut connect_nearby: Option<(String, String, String)> = None;

        // Nearby already filtered in backend to exclude saved clients.
        if state.nearby.is_empty() {
            ui.label(
                egui::RichText::new(
                    "未发现可添加的设备。请确认在同一局域网、双方已启动，且防火墙放行 UDP。",
                )
                .color(t.text_muted)
                .small(),
            );
        } else {
            egui::ScrollArea::vertical()
                .id_salt("nearby_scroll")
                .max_height(140.0)
                .show(ui, |ui| {
                    for d in &state.nearby {
                        let already = state.peers.iter().find(|p| {
                            p.device_id.to_string() == d.device_id || p.addr == d.addr
                        });
                        let connecting = already.map(|p| p.connecting).unwrap_or(false);
                        let auth_failed = already
                            .map(|p| p.status.contains("鉴权") || p.status.contains("密码"))
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
                                            egui::RichText::new("连接中…")
                                                .color(t.warning)
                                                .small(),
                                        );
                                    } else if ui
                                        .add(
                                            egui::Button::new("连接")
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
                                            egui::RichText::new("密码错误")
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
        ui.label(egui::RichText::new("手动添加").strong().color(t.text));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.manual_addr)
                    .desired_width(200.0)
                    .hint_text("192.168.1.10:3721"),
            );
            if ui
                .add(egui::Button::new("连接").fill(t.accent.gamma_multiply(0.45)))
                .clicked()
            {
                state.cmd_add_manual = true;
            }
        });
        ui.label(
            egui::RichText::new("密码正确后才会写入 clients.json 并自动保持连接。")
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
        // Stretch content column like history/devices cards.
        let col_w = ui.available_width().min(520.0);
        ui.set_max_width(ui.available_width());

        ui.label(egui::RichText::new("基本设置").strong().color(t.text));
        ui.add_space(8.0);

        egui::Grid::new("settings")
            .num_columns(2)
            .spacing([16.0, 12.0])
            .min_col_width(110.0)
            .show(ui, |ui| {
                let field_w = (col_w - 130.0).clamp(180.0, 360.0);

                ui.label(egui::RichText::new("设备名称").color(t.text));
                ui.add(
                    egui::TextEdit::singleline(&mut state.device_name).desired_width(field_w),
                );
                ui.end_row();

                ui.label(egui::RichText::new("共享密码").color(t.text));
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
                        "隐藏"
                    } else {
                        "显示"
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
                            "隐藏密码"
                        } else {
                            "显示密码"
                        })
                        .clicked()
                    {
                        state.show_password = !state.show_password;
                    }
                });
                ui.end_row();

                ui.label(egui::RichText::new("TCP 端口").color(t.text));
                ui.add(egui::TextEdit::singleline(&mut state.tcp_port).desired_width(field_w));
                ui.end_row();

                ui.label(egui::RichText::new("UDP 端口").color(t.text));
                ui.add(egui::TextEdit::singleline(&mut state.udp_port).desired_width(field_w));
                ui.end_row();

                ui.label(egui::RichText::new("大小上限 (MiB)").color(t.text));
                ui.add(
                    egui::TextEdit::singleline(&mut state.max_payload_mb).desired_width(field_w),
                );
                ui.end_row();
            });
        ui.label(
            egui::RichText::new(
                "单次同步上限（文本/图片/文件/文件夹打包后）。大文件请设 100–200；协议硬顶约 480 MiB。",
            )
            .small()
            .color(t.text_muted),
        );

        if crate::config::Config::is_insecure_default_password(&state.password) {
            ui.add_space(10.0);
            ui.colored_label(
                egui::Color32::from_rgb(220, 80, 80),
                "⚠ 当前密码为默认值或为空：禁止配对与剪贴板同步。请改成你自己的共享密码。",
            );
        }

        ui.add_space(12.0);
        ui.checkbox(
            &mut state.start_minimized_to_tray,
            egui::RichText::new("启动时最小化到托盘").color(t.text),
        );
        ui.label(
            egui::RichText::new("开启后下次启动只显示托盘图标，不弹出主窗口（左键托盘可打开）。")
                .small()
                .color(t.text_muted),
        );

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new("保存设置")
                        .fill(t.accent.gamma_multiply(0.5))
                        .min_size(egui::vec2(120.0, 32.0)),
                )
                .clicked()
            {
                state.cmd_save_settings = true;
            }
            if ui
                .add(
                    egui::Button::new("打开配置文件夹")
                        .fill(t.card)
                        .min_size(egui::vec2(140.0, 32.0)),
                )
                .on_hover_text("打开 ~/.ohmycopy（配置、历史、inbox）")
                .clicked()
            {
                state.cmd_open_config_folder = true;
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "保存后：密码会热更新（新连接立即生效）。TCP/UDP 端口仅写入配置，必须重启应用后监听/发现才切换。",
            )
            .small()
            .color(t.warning),
        );
        ui.add_space(4.0);
        let cfg_hint = crate::config::Config::config_dir()
            .map(|p| format!("数据目录：{}  （config.json · clients.json · history.db · inbox/）", p.display()))
            .unwrap_or_else(|_| "数据目录：~/.ohmycopy".into());
        ui.label(
            egui::RichText::new(cfg_hint)
                .small()
                .color(t.text_muted),
        );
    });
}
