//! 系统托盘「后台运行」的纯决策逻辑（不依赖任何平台 API）。
//!
//! 平台适配（tray-icon 事件接收、egui 窗口命令）只做薄封装，所有判定
//! 集中在这里并配单元测试：关闭窗口时到底隐藏还是退出、托盘菜单与
//! 图标事件映射成什么动作、悬停提示文案怎么拼。托盘本身的系统行为
//! （图标出现、菜单弹出、提示刷新）只能真机验证，不在这里声称。

/// 窗口关闭请求的处置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    /// 取消关闭并隐藏窗口，进程驻留托盘继续监控。
    HideToTray,
    /// 放行关闭，走正常退出路径（停后端、卸载驱动）。
    Exit,
}

/// 关闭决策：「缩到托盘」开关开启、且这次关闭不是用户明确发起的
/// 退出（托盘菜单/文件菜单的「退出」、程序内部退出）时，缩到托盘；
/// 其余情况一律真退出。
pub fn close_action(minimize_to_tray: bool, explicit_quit: bool) -> CloseAction {
    if minimize_to_tray && !explicit_quit {
        CloseAction::HideToTray
    } else {
        CloseAction::Exit
    }
}

/// 托盘交互动作（适配层把平台事件翻译成本枚举后，GUI 只认它）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// 显示并激活主窗口。
    ShowWindow,
    /// 真正退出程序。
    Quit,
    /// 不处理（右键点击由菜单自身处理、未知菜单项等）。
    None,
}

/// 托盘右键菜单项的固定 ID。
pub const MENU_OPEN_ID: &str = "tray_open";
pub const MENU_QUIT_ID: &str = "tray_quit";

/// 菜单项 ID → 动作。未知 ID 一律忽略，绝不误触发退出。
pub fn menu_action(id: &str) -> TrayAction {
    match id {
        MENU_OPEN_ID => TrayAction::ShowWindow,
        MENU_QUIT_ID => TrayAction::Quit,
        _ => TrayAction::None,
    }
}

/// 托盘图标鼠标事件（适配层从 tray-icon 的事件翻译成本枚举，
/// 只把左键的抬起与双击送进来，其余都归 Other）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconEventKind {
    LeftClick,
    LeftDoubleClick,
    Other,
}

/// 图标事件 → 动作：左键单击与双击都恢复窗口；右键只弹菜单，
/// 不映射动作（菜单项点击走 [`menu_action`]）。
pub fn icon_event_action(kind: IconEventKind) -> TrayAction {
    match kind {
        IconEventKind::LeftClick | IconEventKind::LeftDoubleClick => TrayAction::ShowWindow,
        IconEventKind::Other => TrayAction::None,
    }
}

/// 托盘悬停提示文案：标题一行 + 当前总速率一行。
pub fn tooltip_text(total_down_rate: u64, total_up_rate: u64) -> String {
    format!(
        "流量监控\n下载 {} · 上传 {}",
        humanize_rate(total_down_rate),
        humanize_rate(total_up_rate)
    )
}

/// 与主界面 `humanize_rate` 相同口径的速率格式化（1024 进制）。
/// 本模块必须保持 std-only 以便独立单测，故自带一份。
fn humanize_rate(bps: u64) -> String {
    let b = bps as f64;
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_hides_only_when_enabled_and_not_explicit_quit() {
        assert_eq!(close_action(true, false), CloseAction::HideToTray);
        // 明确退出（托盘/菜单）永远优先于隐藏。
        assert_eq!(close_action(true, true), CloseAction::Exit);
        // 开关关闭时 × 恢复旧行为：真退出。
        assert_eq!(close_action(false, false), CloseAction::Exit);
        assert_eq!(close_action(false, true), CloseAction::Exit);
    }

    #[test]
    fn menu_ids_map_to_actions() {
        assert_eq!(menu_action(MENU_OPEN_ID), TrayAction::ShowWindow);
        assert_eq!(menu_action(MENU_QUIT_ID), TrayAction::Quit);
        assert_eq!(menu_action("tray_unknown"), TrayAction::None);
        assert_eq!(menu_action(""), TrayAction::None);
    }

    #[test]
    fn icon_left_click_and_double_click_show_window() {
        assert_eq!(
            icon_event_action(IconEventKind::LeftClick),
            TrayAction::ShowWindow
        );
        assert_eq!(
            icon_event_action(IconEventKind::LeftDoubleClick),
            TrayAction::ShowWindow
        );
        assert_eq!(icon_event_action(IconEventKind::Other), TrayAction::None);
    }

    #[test]
    fn tooltip_shows_both_rates() {
        assert_eq!(tooltip_text(0, 0), "流量监控\n下载 0 B/s · 上传 0 B/s");
        assert_eq!(
            tooltip_text(1536, 2 * 1024 * 1024),
            "流量监控\n下载 1.5 KB/s · 上传 2.00 MB/s"
        );
    }
}
