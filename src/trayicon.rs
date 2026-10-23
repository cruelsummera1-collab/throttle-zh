//! 系统托盘的平台适配层（tray-icon crate 的薄封装，仅 Windows）。
//!
//! 只做三件事：用程序现有图标建托盘图标与右键菜单；把托盘/菜单
//! 事件从 crate 的全局接收通道里排干、翻译成 [`crate::tray`] 的纯
//! 动作枚举；更新悬停提示。所有判定逻辑都在 `crate::tray`，这里
//! 不做任何决策。托盘句柄必须一直被持有——它一旦被 Drop，托盘
//! 图标就会消失。

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::tray::{
    IconEventKind, MENU_OPEN_ID, MENU_QUIT_ID, TrayAction, icon_event_action, menu_action,
};

/// 托盘句柄（持有即在托盘中显示）。
pub struct TrayHandle {
    tray: TrayIcon,
}

impl TrayHandle {
    /// 创建托盘图标与右键菜单（「打开流量监控」/「退出程序」）。
    /// 失败返回错误字符串，由调用方记日志并降级为无托盘运行
    /// （此时关闭窗口恢复真退出，见 gui 的安全兜底）。
    pub fn new() -> Result<Self, String> {
        let icon = Icon::from_rgba(
            crate::icon::rgba(),
            crate::icon::SIZE,
            crate::icon::SIZE,
        )
        .map_err(|e| format!("托盘图标创建失败：{e}"))?;
        let open_item = MenuItem::with_id(MENU_OPEN_ID, "打开流量监控", true, None);
        let quit_item = MenuItem::with_id(MENU_QUIT_ID, "退出程序", true, None);
        let menu = Menu::with_items(&[&open_item, &quit_item])
            .map_err(|e| format!("托盘菜单创建失败：{e}"))?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_tooltip("流量监控")
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| format!("托盘创建失败：{e}"))?;
        Ok(TrayHandle { tray })
    }

    /// 排干待处理的托盘/菜单事件，翻译成动作列表（无事件时为空）。
    /// 在 GUI 每帧调用；隐藏在托盘里时改以约 1 秒的低频帧调用，
    /// 不空转。
    pub fn poll_actions(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        let menu_rx = MenuEvent::receiver();
        while let Ok(event) = menu_rx.try_recv() {
            let action = menu_action(event.id.as_ref());
            if action != TrayAction::None {
                actions.push(action);
            }
        }
        let icon_rx = TrayIconEvent::receiver();
        while let Ok(event) = icon_rx.try_recv() {
            let kind = match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => IconEventKind::LeftClick,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => IconEventKind::LeftDoubleClick,
                _ => IconEventKind::Other,
            };
            let action = icon_event_action(kind);
            if action != TrayAction::None {
                actions.push(action);
            }
        }
        actions
    }

    /// 更新悬停提示（当前总速率）。失败静默：提示不值得打扰主流程。
    pub fn set_tooltip(&self, text: &str) {
        let _ = self.tray.set_tooltip(Some(text));
    }
}
