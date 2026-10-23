//! egui/eframe front-end for Throttle.
//!
//! Consumes `Snapshot`s from the backend (~1/sec) and emits `Command`s back.
//! This file is built against the shared contract in `crate::types` and must
//! not depend on any backend internals beyond the `snapshot_rx` receiver.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use eframe::egui::Widget;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::backend::rules;
use crate::backend::synthetic::{is_shapable, is_system};
use crate::history::{self, HistoryRecord};
use crate::types::{AlertSettings, Command, ProcessStats, Rule, Snapshot};

/// How many total-rate samples we keep for the header graph.
const TOTAL_HISTORY_LEN: usize = 120;

/// Project links used by the Help menu and the About dialog.
const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version of the bundled WinDivert driver (see `vendor/`).
const WINDIVERT_VERSION: &str = "2.2.2";

const COLOR_DOWN: egui::Color32 = egui::Color32::from_rgb(0x4c, 0xaf, 0x50);
const COLOR_UP: egui::Color32 = egui::Color32::from_rgb(0x21, 0x96, 0xf3);
const COLOR_BLOCK: egui::Color32 = egui::Color32::from_rgb(0xe5, 0x39, 0x35);

// ---------------------------------------------------------------------------
// Formatting / parsing helpers
// ---------------------------------------------------------------------------

const KB: f64 = 1024.0;
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;
const TB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

/// Humanize a byte-per-second rate using base-1024 units.
fn humanize_rate(bps: u64) -> String {
    let b = bps as f64;
    if b >= GB {
        format!("{:.2} GB/s", b / GB)
    } else if b >= MB {
        format!("{:.2} MB/s", b / MB)
    } else if b >= KB {
        format!("{:.1} KB/s", b / KB)
    } else {
        format!("{bps} B/s")
    }
}

/// Humanize a plain byte total using base-1024 units.
fn humanize_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= TB {
        format!("{:.2} TB", b / TB)
    } else if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Rate unit for the limit dialog's dropdown (base 1024).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RateUnit {
    Bps,
    KBps,
    MBps,
    GBps,
}

impl RateUnit {
    const ALL: [RateUnit; 4] = [
        RateUnit::Bps,
        RateUnit::KBps,
        RateUnit::MBps,
        RateUnit::GBps,
    ];

    fn label(self) -> &'static str {
        match self {
            RateUnit::Bps => "B/s",
            RateUnit::KBps => "KB/s",
            RateUnit::MBps => "MB/s",
            RateUnit::GBps => "GB/s",
        }
    }

    fn multiplier(self) -> f64 {
        match self {
            RateUnit::Bps => 1.0,
            RateUnit::KBps => KB,
            RateUnit::MBps => MB,
            RateUnit::GBps => GB,
        }
    }

    /// Split a byte rate into (value, unit) using the largest unit >= 1.
    fn decompose(bps: u64) -> (f64, RateUnit) {
        let b = bps as f64;
        if b >= GB {
            (b / GB, RateUnit::GBps)
        } else if b >= MB {
            (b / MB, RateUnit::MBps)
        } else if b >= KB {
            (b / KB, RateUnit::KBps)
        } else {
            (b, RateUnit::Bps)
        }
    }
}

/// One-line summary of a rule for the table column.
fn rule_summary(rule: Option<&Rule>) -> String {
    match rule {
        None => "-".to_string(),
        Some(r) if r.blocked => "已阻断".to_string(),
        Some(r) => {
            let mut parts = Vec::new();
            if let Some(d) = r.down_limit {
                parts.push(format!("下载限速 {}", humanize_rate(d)));
            }
            if let Some(u) = r.up_limit {
                parts.push(format!("上传限速 {}", humanize_rate(u)));
            }
            if parts.is_empty() {
                "-".to_string()
            } else {
                parts.join("  ")
            }
        }
    }
}

/// 合成行的中文显示名（后端给的名字是英文 Unknown/System）。
fn display_process_name(name: &str, key_or_path: &str) -> String {
    match key_or_path {
        "<unknown>" => "未归属流量".to_string(),
        "system" => "系统".to_string(),
        _ => name.to_string(),
    }
}

/// 把秒数格式化为"X 分 Y 秒"/"Y 秒"。
fn format_duration(secs: u64) -> String {
    if secs >= 60 {
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    } else {
        format!("{secs} 秒")
    }
}

/// 注册 Windows 系统中文字体作为回退字体（不打包任何字体文件）。
///
/// egui 默认字体不含中文字形；把系统字体追加到字体族末尾后，拉丁字符
/// 仍用默认字体，中文自动回退到系统字体，显示不再出现方框。
fn install_chinese_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "C:\\Windows\\Fonts\\msyh.ttc",   // 微软雅黑
        "C:\\Windows\\Fonts\\simhei.ttf", // 黑体
        "C:\\Windows\\Fonts\\simsun.ttc", // 宋体
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        // 只接受 TTF/TTC/OTF 魔数，避免把损坏文件喂给字体引擎。
        let magic_ok = bytes.starts_with(b"ttcf")
            || bytes.starts_with(b"OTTO")
            || bytes.starts_with(&[0x00, 0x01, 0x00, 0x00]);
        if !magic_ok {
            continue;
        }
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "zh_fallback".to_owned(),
            egui::FontData::from_owned(bytes).into(),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.push("zh_fallback".to_owned());
            }
        }
        ctx.set_fonts(fonts);
        tracing::info!("已加载中文字体回退：{path}");
        return;
    }
    tracing::warn!("未找到可用的系统中文字体，中文可能显示为方框");
}

// ---------------------------------------------------------------------------
// Sorting / modal state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortColumn {
    Name,
    Flows,
    Down,
    Up,
    DownTotal,
    UpTotal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Down,
    Up,
}

/// State for the small "set limit" modal window.
struct LimitModal {
    exe_path: String,
    name: String,
    direction: Direction,
    /// Numeric value as typed (empty = unlimited).
    value: String,
    unit: RateUnit,
    error: Option<String>,
}

/// 告警设置窗口的编辑状态（打开时从后端快照复制一份，保存才下发）。
struct AlertDraft {
    threshold_value: String,
    threshold_unit: RateUnit,
    duration_secs: String,
    /// 忽略名单，每行一个进程名或完整路径。
    ignore_text: String,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// The app
// ---------------------------------------------------------------------------

pub struct ThrottleApp {
    snapshot_rx: Receiver<Snapshot>,
    cmd_tx: Sender<Command>,

    last_snapshot: Snapshot,
    total_down_hist: Vec<f64>,
    total_up_hist: Vec<f64>,

    sort_column: SortColumn,
    sort_desc: bool,
    selected: Option<String>,

    limit_modal: Option<LimitModal>,
    about_open: bool,
    shutdown_sent: bool,

    alert_open: bool,
    alert_draft: Option<AlertDraft>,
    history_open: bool,
    history_rows: Vec<HistoryRecord>,
}

impl ThrottleApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        snapshot_rx: Receiver<Snapshot>,
        cmd_tx: Sender<Command>,
    ) -> Self {
        install_chinese_fonts(&cc.egui_ctx);
        Self {
            snapshot_rx,
            cmd_tx,
            last_snapshot: Snapshot::default(),
            total_down_hist: Vec::with_capacity(TOTAL_HISTORY_LEN),
            total_up_hist: Vec::with_capacity(TOTAL_HISTORY_LEN),
            sort_column: SortColumn::Down,
            sort_desc: true,
            selected: None,
            limit_modal: None,
            about_open: false,
            shutdown_sent: false,
            alert_open: false,
            alert_draft: None,
            history_open: false,
            history_rows: Vec::new(),
        }
    }

    fn send_shutdown(&mut self) {
        if !self.shutdown_sent {
            let _ = self.cmd_tx.send(Command::Shutdown);
            self.shutdown_sent = true;
        }
    }

    /// The current rule for `key`, or a fresh default rule bound to that path.
    fn base_rule(&self, key: &str) -> Rule {
        self.last_snapshot
            .rules
            .get(key)
            .cloned()
            .unwrap_or_else(|| Rule {
                exe_path: key.to_string(),
                ..Rule::default()
            })
    }

    fn send_rule(&mut self, rule: Rule) {
        let _ = self.cmd_tx.send(Command::SetRule(rule));
    }

    fn set_blocked(&mut self, key: &str, blocked: bool) {
        let mut rule = self.base_rule(key);
        rule.exe_path = key.to_string();
        rule.blocked = blocked;
        self.send_rule(rule);
    }

    fn open_limit_modal(&mut self, key: &str, name: &str, direction: Direction) {
        let existing = self.last_snapshot.rules.get(key);
        let current = existing.and_then(|r| match direction {
            Direction::Down => r.down_limit,
            Direction::Up => r.up_limit,
        });
        let (value, unit) = match current {
            Some(bps) => {
                let (v, u) = RateUnit::decompose(bps);
                // Trim trailing zeros for a clean editable number.
                let s = format!("{v:.2}");
                let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
                (s, u)
            }
            None => (String::new(), RateUnit::MBps),
        };
        self.limit_modal = Some(LimitModal {
            exe_path: key.to_string(),
            name: name.to_string(),
            direction,
            value,
            unit,
            error: None,
        });
    }

    /// Adjust the style to make sort headers look less like regular buttons.
    /// This only affects the local Ui (the Grid), not Menus or Modals.
    /// TODO: Looks ugly and could affect unrelated buttons, replace with "classes" in egui 0.37+:
    ///   https://github.com/emilk/egui/pull/8153
    ///   https://github.com/emilk/egui/blob/7ba3db/examples/styling_engine/src/main.rs
    fn sort_header_button_style(&self, ui: &mut egui::Ui, col_selected: bool) {
        let style = ui.style_mut();
        let widgets_style = &mut style.visuals.widgets;

        let transparent = egui::Color32::TRANSPARENT;
        // Use a gray-ish luminance (0.3) with variable opacity -> works for light and dark theme
        let selected = egui::Rgba::from_luminance_alpha(0.3, 0.15).into();
        let hovered = egui::Rgba::from_luminance_alpha(0.3, 0.25).into();

        widgets_style.active.weak_bg_fill = hovered; // "active" = while being clicked
        widgets_style.hovered.weak_bg_fill = hovered;
        widgets_style.inactive.weak_bg_fill = if col_selected { selected } else { transparent };

        widgets_style.active.bg_stroke = egui::Stroke::NONE;
        widgets_style.hovered.bg_stroke = egui::Stroke::NONE;
        widgets_style.inactive.bg_stroke = egui::Stroke::NONE;
    }

    /// Adjust the sort state when a header is clicked.
    fn sort_header(&mut self, ui: &mut egui::Ui, label: &str, col: SortColumn) {
        let text = egui::RichText::new(label).strong();
        let grow = egui::Atom::grow();
        let col_selected = self.sort_column == col;

        let direction_text = if !col_selected {
            ""
        } else if self.sort_desc {
            "▼"
        } else {
            "▲"
        };
        // The default font is missing the arrows, use the bundled monospace font (Hack) instead
        let direction = egui::RichText::new(direction_text).monospace();

        self.sort_header_button_style(ui, col_selected);
        let button = egui::Button::new((text, grow))
            .right_text(direction)
            .min_size(ui.available_size())
            .corner_radius(0);

        if ui.add(button).clicked() {
            if col_selected {
                self.sort_desc = !self.sort_desc;
            } else {
                self.sort_column = col;
                self.sort_desc = true;
            }
        }
    }

    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("打开规则文件").clicked() {
                        open_path(&rules::rules_path());
                        ui.close();
                    }
                    if ui.button("打开日志文件").clicked() {
                        open_path(&rules::config_dir().join("throttle.log"));
                        ui.close();
                    }
                    if ui.button("打开数据文件夹").clicked() {
                        open_path(&rules::config_dir());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("退出").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });
                ui.menu_button("帮助", |ui| {
                    if ui.button("使用文档").clicked() {
                        open_url(ui.ctx(), &format!("{REPO_URL}#readme"));
                        ui.close();
                    }
                    if ui.button("反馈问题").clicked() {
                        open_url(ui.ctx(), &format!("{REPO_URL}/issues/new"));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("查看许可证").clicked() {
                        open_url(ui.ctx(), &format!("{REPO_URL}/blob/main/LICENSE"));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("关于").clicked() {
                        self.about_open = true;
                        ui.close();
                    }
                });
                ui.separator();
                if ui.button("告警设置").clicked() {
                    self.open_alert_settings();
                }
                if ui.button("历史记录").clicked() {
                    self.open_history();
                }
            });
        });
    }

    /// 持续高上传告警横幅：只要后端报告有正在持续的告警就显示。
    fn draw_alert_banner(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let Some(alert) = &snap.active_alert else {
            return;
        };
        egui::Panel::top("alert_banner").show(ui, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(0x7f, 0x1d, 0x1d))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "持续高上传提醒：{}（PID {}）正在以 {} 上传，已持续 {}（阈值 {}，触发于 {}）。\
                             这只说明该程序在持续上传数据，是否有问题请结合它的实际用途判断。",
                            display_process_name(&alert.name, &alert.exe_path),
                            alert.pid,
                            humanize_rate(alert.up_rate),
                            format_duration(alert.sustained_secs),
                            humanize_rate(alert.threshold_bps),
                            alert.triggered_at,
                        ))
                        .color(egui::Color32::WHITE)
                        .strong(),
                    );
                });
        });
    }

    fn open_alert_settings(&mut self) {
        let s = &self.last_snapshot.alert_settings;
        let (value, unit) = if s.up_threshold_bps == 0 {
            (0.0, RateUnit::KBps)
        } else {
            RateUnit::decompose(s.up_threshold_bps)
        };
        // Trim trailing zeros for a clean editable number.
        let text = format!("{value:.2}");
        let text = text.trim_end_matches('0').trim_end_matches('.').to_string();
        self.alert_draft = Some(AlertDraft {
            threshold_value: text,
            threshold_unit: unit,
            duration_secs: s.duration_secs.to_string(),
            ignore_text: s.ignore.join("\n"),
            error: None,
        });
        self.alert_open = true;
    }

    fn draw_alert_settings(&mut self, ctx: &egui::Context) {
        if !self.alert_open {
            return;
        }
        let Some(mut draft) = self.alert_draft.take() else {
            self.alert_open = false;
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("异常上传提醒设置")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "当某个进程的上传速度连续达到阈值并持续满设定时间后，\
                     主界面顶部会弹出提醒，事件也会记入历史记录。短暂的瞬时峰值不会触发。",
                );
                ui.add_space(6.0);
                egui::Grid::new("alert_settings_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("上传速度阈值");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.threshold_value)
                                    .desired_width(80.0)
                                    .hint_text("例如 500"),
                            );
                            egui::ComboBox::from_id_salt("alert-rate-unit")
                                .selected_text(draft.threshold_unit.label())
                                .show_ui(ui, |ui| {
                                    for u in RateUnit::ALL {
                                        ui.selectable_value(
                                            &mut draft.threshold_unit,
                                            u,
                                            u.label(),
                                        );
                                    }
                                });
                        });
                        ui.end_row();
                        ui.label("持续时间（秒）");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.duration_secs)
                                .desired_width(80.0)
                                .hint_text("例如 30"),
                        );
                        ui.end_row();
                    });
                ui.label(
                    egui::RichText::new("阈值填 0 表示关闭提醒；持续时间范围 1 到 3600 秒。")
                        .weak()
                        .small(),
                );
                ui.separator();
                ui.label("忽略名单（每行一个进程名或完整路径，不区分大小写；留空表示不忽略任何程序）：");
                ui.add(
                    egui::TextEdit::multiline(&mut draft.ignore_text)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("例如：trusted.exe"),
                );
                if let Some(err) = &draft.error {
                    ui.colored_label(COLOR_BLOCK, err);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        save = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel || !open {
            self.alert_open = false;
            return;
        }
        if save {
            let thr_text = draft.threshold_value.trim().replace(',', ".");
            let thr = if thr_text.is_empty() {
                Err("请填写上传速度阈值（填 0 表示关闭提醒）".to_string())
            } else {
                match thr_text.parse::<f64>() {
                    Ok(v) if v.is_finite() && v >= 0.0 => {
                        Ok((v * draft.threshold_unit.multiplier()) as u64)
                    }
                    _ => Err("阈值必须是数字，例如 500 或 0.5".to_string()),
                }
            };
            let dur = match draft.duration_secs.trim().parse::<u32>() {
                Ok(d) if (1..=3600).contains(&d) => Ok(d),
                _ => Err("持续时间必须是 1 到 3600 之间的整数（秒）".to_string()),
            };
            match (thr, dur) {
                (Ok(t), Ok(d)) => {
                    let settings = AlertSettings {
                        up_threshold_bps: t,
                        duration_secs: d,
                        ignore: draft
                            .ignore_text
                            .lines()
                            .map(|l| l.trim().to_lowercase())
                            .filter(|l| !l.is_empty())
                            .collect(),
                    };
                    let _ = self.cmd_tx.send(Command::SetAlertSettings(settings));
                    self.alert_open = false;
                    return;
                }
                (Err(e), _) | (_, Err(e)) => {
                    draft.error = Some(e);
                }
            }
        }
        self.alert_draft = Some(draft);
    }

    fn open_history(&mut self) {
        self.history_rows =
            history::read_recent(&history::history_path(&rules::config_dir()), 500);
        self.history_open = true;
    }

    fn draw_history(&mut self, ctx: &egui::Context) {
        if !self.history_open {
            return;
        }
        let path = history::history_path(&rules::config_dir());
        let mut open = true;
        egui::Window::new("历史记录")
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 520.0])
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("刷新").clicked() {
                        self.history_rows = history::read_recent(&path, 500);
                    }
                    ui.label(
                        egui::RichText::new(format!("文件位置：{}", path.display()))
                            .weak()
                            .small(),
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "每分钟记录一次各进程该分钟内的流量增量，告警事件单独记录；保留最近 30 天。\
                         这里的累计与主界面的“本次运行累计”口径不同，不要相加。",
                    )
                    .weak()
                    .small(),
                );
                ui.separator();
                ui.heading("告警事件");
                let alerts: Vec<&HistoryRecord> = self
                    .history_rows
                    .iter()
                    .filter(|r| r.kind == "alert")
                    .rev()
                    .take(50)
                    .collect();
                if alerts.is_empty() {
                    ui.label("暂无告警事件。");
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("alert_history_scroll")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for r in alerts {
                                ui.label(format!(
                                    "{}  {}（PID {}）上传 {}，阈值 {}，已持续 {}",
                                    r.time,
                                    display_process_name(&r.name, &r.exe_path),
                                    r.pid,
                                    humanize_rate(r.up_rate),
                                    humanize_rate(r.threshold_bps),
                                    format_duration(r.sustained_secs),
                                ));
                                if !r.exe_path.is_empty() {
                                    ui.label(egui::RichText::new(&r.exe_path).weak().small());
                                }
                            }
                        });
                }
                ui.separator();
                ui.heading("每分钟流量记录（最新在前）");
                egui::ScrollArea::vertical()
                    .id_salt("minute_history_scroll")
                    .show(ui, |ui| {
                        for r in self
                            .history_rows
                            .iter()
                            .filter(|r| r.kind != "alert")
                            .rev()
                            .take(200)
                        {
                            ui.label(format!(
                                "{}  {}  下载 {} / 上传 {}",
                                r.time,
                                display_process_name(&r.name, &r.exe_path),
                                humanize_bytes(r.down_bytes),
                                humanize_bytes(r.up_bytes),
                            ));
                        }
                    });
            });
        self.history_open = open;
    }

    fn draw_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("关于")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(6.0);
                    ui.heading("流量监控");
                    ui.label(egui::RichText::new(format!("版本 {APP_VERSION}")).strong());
                    ui.add_space(4.0);
                    ui.label(env!("CARGO_PKG_DESCRIPTION"));
                    ui.add_space(8.0);
                });
                ui.separator();
                egui::Grid::new("about_grid").num_columns(2).show(ui, |ui| {
                    ui.label("抓包驱动");
                    ui.label(format!("WinDivert {WINDIVERT_VERSION}"));
                    ui.end_row();
                    ui.label("界面框架");
                    ui.label("egui / eframe");
                    ui.end_row();
                    ui.label("许可证");
                    ui.label(env!("CARGO_PKG_LICENSE"));
                    ui.end_row();
                    ui.label("数据目录");
                    ui.label(
                        egui::RichText::new(rules::config_dir().display().to_string())
                            .weak()
                            .small(),
                    );
                    ui.end_row();
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.hyperlink_to("GitHub", REPO_URL);
                    ui.hyperlink_to("发布页", format!("{REPO_URL}/releases"));
                    ui.hyperlink_to("反馈问题", format!("{REPO_URL}/issues"));
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Copyright © 2026 Francesco Gruosso")
                        .weak()
                        .small(),
                );
            });
        self.about_open = open;
    }

    fn draw_top_panel(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        egui::Panel::top("top_panel")
            .resizable(true)
            .default_size(120.0)
            .min_size(70.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading("流量监控");
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!(
                            "下载 {}",
                            humanize_rate(snap.total_down_rate)
                        ))
                        .color(COLOR_DOWN)
                        .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!("上传 {}", humanize_rate(snap.total_up_rate)))
                            .color(COLOR_UP)
                            .strong(),
                    );
                    ui.separator();
                    ui.label(format!("{} 个进程", snap.processes.len()));
                });

                // Let the plot fill the panel's remaining height rather than
                // requesting an explicit one. An explicit height derived from
                // available_height() makes the content's reported size disagree
                // with the drag target each frame, which egui persists as the new
                // panel size and turns into a resize feedback loop. Filling avoids
                // that.
                Plot::new("total_plot")
                    .show_x(false)
                    .allow_zoom(false)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .legend(Legend::default())
                    .include_y(0.0)
                    .show(ui, |pui| {
                        pui.line(
                            Line::new("下载", PlotPoints::from_ys_f64(&self.total_down_hist))
                                .color(COLOR_DOWN),
                        );
                        pui.line(
                            Line::new("上传", PlotPoints::from_ys_f64(&self.total_up_hist))
                                .color(COLOR_UP),
                        );
                    });
            });
    }

    fn draw_detail_panel(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        // Always visible; shows a hint until a process is selected.
        let stats = self
            .selected
            .as_ref()
            .and_then(|key| snap.processes.get(key).cloned());

        egui::Panel::right("detail_panel")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                let Some(stats) = stats else {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("从列表选择一个进程查看详情")
                                .weak()
                                .italics(),
                        );
                    });
                    return;
                };
                let key = stats.exe_path.clone();
                ui.heading(display_process_name(&stats.name, &key));
                ui.label(egui::RichText::new(&stats.exe_path).weak().small());
                ui.separator();

                egui::Grid::new("detail_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("PID");
                        ui.label(stats.pid.to_string());
                        ui.end_row();
                        ui.label("连接数");
                        ui.label(stats.flow_count.to_string());
                        ui.end_row();
                        ui.label("下载速度");
                        ui.label(
                            egui::RichText::new(humanize_rate(stats.down_rate)).color(COLOR_DOWN),
                        );
                        ui.end_row();
                        ui.label("上传速度");
                        ui.label(egui::RichText::new(humanize_rate(stats.up_rate)).color(COLOR_UP));
                        ui.end_row();
                        ui.label("累计下载（本次运行）");
                        ui.label(humanize_bytes(stats.down_total));
                        ui.end_row();
                        ui.label("累计上传（本次运行）");
                        ui.label(humanize_bytes(stats.up_total));
                        ui.end_row();
                        ui.label("规则");
                        ui.label(rule_summary(snap.rules.get(&key)));
                        ui.end_row();
                    });

                ui.separator();
                ui.label("最近 60 秒速率（每秒）");

                let down: Vec<f64> = stats.down_history.iter().map(|&v| v as f64).collect();
                let up: Vec<f64> = stats.up_history.iter().map(|&v| v as f64).collect();
                Plot::new("detail_plot")
                    .height(160.0)
                    .show_x(false)
                    .allow_zoom(false)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .legend(Legend::default())
                    .include_y(0.0)
                    .show(ui, |pui| {
                        pui.line(
                            Line::new("下载", PlotPoints::from_ys_f64(&down)).color(COLOR_DOWN),
                        );
                        pui.line(Line::new("上传", PlotPoints::from_ys_f64(&up)).color(COLOR_UP));
                    });
            });
    }

    fn draw_table(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        // Owned, sorted list of (lowercase key, stats). The key is used for all
        // rule commands per the shared contract.
        let mut rows: Vec<(String, ProcessStats)> = snap
            .processes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let sort_column = self.sort_column;
        let sort_desc = self.sort_desc;
        rows.sort_by(|a, b| {
            let (a, b) = (&a.1, &b.1);
            let ord = match sort_column {
                SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortColumn::Flows => a.flow_count.cmp(&b.flow_count),
                SortColumn::Down => a.down_rate.cmp(&b.down_rate),
                SortColumn::Up => a.up_rate.cmp(&b.up_rate),
                SortColumn::DownTotal => a.down_total.cmp(&b.down_total),
                SortColumn::UpTotal => a.up_total.cmp(&b.up_total),
            };
            if sort_desc { ord.reverse() } else { ord }
        });

        const SORT_COLUMN_LABELS: [(SortColumn, &str); 6] = [
            (SortColumn::Name, "进程"),
            (SortColumn::Flows, "连接"),
            (SortColumn::Down, "下载"),
            (SortColumn::Up, "上传"),
            (SortColumn::DownTotal, "累计下载"),
            (SortColumn::UpTotal, "累计上传"),
        ];

        egui::CentralPanel::default().show(ui, |ui| {
            egui_extras::TableBuilder::new(ui)
                .resizable(true)
                .auto_shrink(false)
                .striped(true)
                .sense(egui::Sense::CLICK | egui::Sense::HOVER)
                .column(egui_extras::Column::remainder().at_least(200.)) // Name
                .columns(egui_extras::Column::auto(), 5) // Numeric
                .column(egui_extras::Column::remainder()) // Rule
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .header(20., |mut row| {
                    for (sort_column, label) in SORT_COLUMN_LABELS {
                        row.col(|ui| self.sort_header(ui, label, sort_column));
                    }
                    row.col(|ui| {
                        ui.label(egui::RichText::new("规则").strong());
                    });
                })
                .body(|body| {
                    body.rows(20., rows.len(), |row| {
                        let (key, stats) = rows.get(row.index()).expect("row index in bounds");

                        let resp = self.draw_table_row(row, key, stats, snap);
                        if resp.clicked() || resp.secondary_clicked() {
                            self.selected = Some(key.clone());
                        }
                        let shown_name = display_process_name(&stats.name, key);
                        self.row_context_menu(&resp, key, &shown_name);
                    });
                });
        });
    }

    fn draw_table_row(
        &mut self,
        mut row: egui_extras::TableRow,
        key: &String,
        stats: &ProcessStats,
        snap: &Snapshot,
    ) -> egui::Response {
        let selected = self.selected.as_deref() == Some(key.as_str());

        let rule = snap.rules.get(key);
        let summary = rule_summary(rule);
        let rule_text = if rule.map(|r| r.blocked).unwrap_or(false) {
            egui::RichText::new(summary).color(COLOR_BLOCK).strong()
        } else {
            egui::RichText::new(summary)
        };

        let cells: [egui::RichText; 7] = [
            egui::RichText::new(display_process_name(&stats.name, key)),
            egui::RichText::new(stats.flow_count.to_string()),
            egui::RichText::new(humanize_rate(stats.down_rate)).color(COLOR_DOWN),
            egui::RichText::new(humanize_rate(stats.up_rate)).color(COLOR_UP),
            egui::RichText::new(humanize_bytes(stats.down_total)),
            egui::RichText::new(humanize_bytes(stats.up_total)),
            rule_text,
        ];
        row.set_selected(selected);
        for content in cells {
            row.col(|ui| {
                egui::Label::new(content)
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .selectable(false)
                    .ui(ui);
            });
        }

        row.response()
    }

    fn row_context_menu(&mut self, resp: &egui::Response, key: &str, name: &str) {
        resp.context_menu(|ui| {
            // The "Unknown" row is not a process but the traffic we failed to
            // attribute, so it gets no rule actions.
            if !is_shapable(key) {
                ui.label(
                    egui::RichText::new("未归属流量，无法设置规则")
                        .weak()
                        .italics(),
                );
                return;
            }
            // System is shapable, but covers more than people expect.
            if is_system(key) {
                ui.label(
                    egui::RichText::new(
                        "系统流量：包括 VPN 隧道、网络共享和 Windows 更新",
                    )
                    .weak()
                    .small(),
                );
                ui.separator();
            }
            if ui.button("设置下载限速…").clicked() {
                self.open_limit_modal(key, name, Direction::Down);
                ui.close();
            }
            if ui.button("设置上传限速…").clicked() {
                self.open_limit_modal(key, name, Direction::Up);
                ui.close();
            }
            ui.separator();
            let blocked = self
                .last_snapshot
                .rules
                .get(key)
                .map(|r| r.blocked)
                .unwrap_or(false);
            if blocked {
                if ui.button("解除阻断").clicked() {
                    self.set_blocked(key, false);
                    ui.close();
                }
            } else if ui.button("阻断").clicked() {
                self.set_blocked(key, true);
                ui.close();
            }
            ui.separator();
            if ui.button("删除规则").clicked() {
                let _ = self.cmd_tx.send(Command::RemoveRule(key.to_string()));
                ui.close();
            }
        });
    }

    fn draw_limit_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.limit_modal.take() else {
            return;
        };

        let dir_label = match modal.direction {
            Direction::Down => "下载",
            Direction::Up => "上传",
        };

        let mut open = true;
        let mut apply = false;
        let mut cancel = false;

        egui::Window::new(format!("设置{dir_label}限速"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("进程：{}", modal.name));
                ui.label(
                    egui::RichText::new("留空表示不限速。")
                        .weak()
                        .small(),
                );
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut modal.value)
                            .desired_width(80.0)
                            .hint_text("例如 2"),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply = true;
                    }
                    egui::ComboBox::from_id_salt("rate-unit")
                        .selected_text(modal.unit.label())
                        .show_ui(ui, |ui| {
                            for u in RateUnit::ALL {
                                ui.selectable_value(&mut modal.unit, u, u.label());
                            }
                        });
                });
                if let Some(err) = &modal.error {
                    ui.colored_label(COLOR_BLOCK, err);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel || !open {
            return; // modal dropped
        }

        if apply {
            let trimmed = modal.value.trim().replace(',', ".");
            let limit: Option<u64> = if trimmed.is_empty() {
                None
            } else {
                match trimmed.parse::<f64>() {
                    Ok(v) if v.is_finite() && v >= 0.0 => {
                        Some((v * modal.unit.multiplier()) as u64)
                    }
                    _ => {
                        modal.error = Some("请输入数字，例如 2 或 0.5".to_string());
                        self.limit_modal = Some(modal);
                        return;
                    }
                }
            };
            let mut rule = self.base_rule(&modal.exe_path);
            rule.exe_path = modal.exe_path.clone();
            match modal.direction {
                Direction::Down => rule.down_limit = limit,
                Direction::Up => rule.up_limit = limit,
            }
            self.send_rule(rule);
            return; // done, modal closes
        }

        // Still open, no decision yet: keep it around.
        self.limit_modal = Some(modal);
    }
}

impl eframe::App for ThrottleApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Drain any pending snapshots each frame.
        let incoming: Vec<Snapshot> = self.snapshot_rx.try_iter().collect();
        for snap in incoming {
            self.total_down_hist.push(snap.total_down_rate as f64);
            self.total_up_hist.push(snap.total_up_rate as f64);
            if self.total_down_hist.len() > TOTAL_HISTORY_LEN {
                let excess = self.total_down_hist.len() - TOTAL_HISTORY_LEN;
                self.total_down_hist.drain(0..excess);
            }
            if self.total_up_hist.len() > TOTAL_HISTORY_LEN {
                let excess = self.total_up_hist.len() - TOTAL_HISTORY_LEN;
                self.total_up_hist.drain(0..excess);
            }
            self.last_snapshot = snap;
        }

        // Window close -> tell the backend to shut down.
        if ctx.input(|i| i.viewport().close_requested()) {
            self.send_shutdown();
        }

        let snap = self.last_snapshot.clone();

        // Clear a stale selection so the side panel doesn't linger.
        if let Some(sel) = &self.selected
            && !snap.processes.contains_key(sel)
        {
            self.selected = None;
        }

        self.draw_menu_bar(ui);
        self.draw_alert_banner(ui, &snap);
        self.draw_top_panel(ui, &snap);
        self.draw_detail_panel(ui, &snap);
        self.draw_table(ui, &snap);
        self.draw_limit_modal(&ctx);
        self.draw_alert_settings(&ctx);
        self.draw_history(&ctx);
        self.draw_about(&ctx);

        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl Drop for ThrottleApp {
    fn drop(&mut self) {
        self.send_shutdown();
    }
}

// ---------------------------------------------------------------------------
// Shell helpers
// ---------------------------------------------------------------------------

/// Open a URL in the default browser.
fn open_url(ctx: &egui::Context, url: &str) {
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

/// Open a file with its default application, or a folder in Explorer.
/// Best-effort: failures are logged, never surfaced as errors.
fn open_path(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("explorer").arg(path).spawn() {
        tracing::warn!("failed to open {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------------
// Fallback error window (shown when the backend can't start, e.g. not elevated
// or WinDivert is missing).
// ---------------------------------------------------------------------------

pub struct ErrorApp {
    message: String,
}

impl ErrorApp {
    pub fn new(message: String) -> Self {
        Self { message }
    }
}

impl eframe::App for ErrorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.heading("流量监控无法启动");
            });
            ui.add_space(12.0);
            ui.label(
                "后端初始化失败。常见原因：程序没有以管理员身份运行，\
                 或者 WinDivert 驱动文件（WinDivert.dll / WinDivert64.sys）\
                 不在程序所在的同一目录下。请确认后重新以管理员身份运行。",
            );
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(egui::RichText::new("详细信息：").strong());
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(
                    egui::RichText::new(&self.message)
                        .monospace()
                        .color(COLOR_BLOCK),
                );
            });
            ui.add_space(12.0);
            ui.separator();
            if ui.button("关闭").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}
