//! 启动失败页的分类、文案决策与「以管理员身份重新启动」（阶段 14）。
//!
//! 背景：开机自启动只能以普通权限拉起程序，而抓包必须管理员权限，
//! 于是自启必然落在启动失败页。失败页要能自救：当失败原因确实是
//! 「没有管理员权限」时，提供一个提权重启按钮，走 ShellExecute 的
//! runas 动词弹 UAC；文件缺失等其他失败提权也救不了，不显示按钮。
//!
//! 单实例配合：当前实例持有全局互斥量，新实例若同刻抢锁会被挡回。
//! 顺序定为：提权拉起时给新实例带 [`RELAUNCH_ARG`] 参数，旧实例在
//! ShellExecute 成功后立即退出（进程退出即释放互斥量）；新实例见
//! 此参数时改用「等待重试」方式抢锁（见 single_instance::acquire_wait），
//! 等旧实例让位后正常进入主界面。普通双击启动不带此参数，抢锁行为
//! 与从前完全一致。
//!
//! 本模块的分类/决策/结果映射都是纯函数，可独立单元测试；
//! ShellExecute 调用只在 Windows 编译。

/// 提权重启时传给新实例的参数：提示它抢单实例锁时等待旧实例退出。
pub const RELAUNCH_ARG: &str = "--elevated-relaunch";

/// 命令行里是否带提权重启参数。
pub fn has_relaunch_arg(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter().any(|a| a.as_ref() == RELAUNCH_ARG)
}

/// 后端启动失败的分类。分类本身（对 anyhow 错误链的 downcast）在
/// main.rs 完成，这里只消费分类结果做决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// WinDivert 打开被拒（错误 5）：没有管理员权限。提权可解。
    NotElevated,
    /// WinDivert 驱动文件缺失（错误 2，SYS 文件找不到）。
    /// 注：WinDivert.dll 缺失时进程在加载期就起不来，走不到失败页，
    /// 能到这里的只有 .sys 缺失。
    MissingDriverFiles,
    /// 其他失败（驱动被拦、版本不兼容、BFE 服务关闭等）：提权无用，
    /// 且进程可能本就已是管理员身份，再弹提权按钮只会死循环。
    Other,
}

/// 失败页对某个分类的呈现决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FailureActions {
    /// 是否显示「以管理员身份重新启动」按钮。
    pub show_elevate_button: bool,
    /// 失败页正文（全中文，把原因与下一步说清）。
    pub body_text: &'static str,
}

/// 按分类决定失败页的正文与按钮。
pub fn actions_for(kind: FailureKind) -> FailureActions {
    match kind {
        FailureKind::NotElevated => FailureActions {
            show_elevate_button: true,
            body_text: "程序没有以管理员身份运行，无法加载抓包驱动，所以打不开。\n\
                       如果这个窗口是开机自启动弹出来的，属于正常现象——自启动只能用普通权限拉起程序。\n\
                       点下方「以管理员身份重新启动」，在 Windows 弹窗里选择「是」，程序就会以管理员身份重新打开。",
        },
        FailureKind::MissingDriverFiles => FailureActions {
            show_elevate_button: false,
            body_text: "没有找到 WinDivert 驱动文件（WinDivert64.sys），无法加载抓包驱动。\n\
                       请确认 WinDivert.dll 与 WinDivert64.sys 和程序放在同一个目录下，补齐后重新运行。\n\
                       这是文件缺失，改用管理员身份运行也解决不了，所以这里不提供提权重启按钮。",
        },
        FailureKind::Other => FailureActions {
            show_elevate_button: false,
            body_text: "后端初始化失败，原因见下方详细信息。常见情况：抓包驱动被安全软件拦截、\
                       驱动版本不兼容、或系统的 Base Filtering Engine 服务被关闭。\n\
                       这类问题改用管理员身份运行通常也解决不了，请对照详细信息处理后重新运行。",
        },
    }
}

/// 提权重启的一次尝试结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevateOutcome {
    /// 新实例已拉起：当前实例应立即退出，把单实例锁让给它。
    Launched,
    /// 用户在 UAC 弹窗点了「否」（ShellExecute 返回 SE_ERR_ACCESSDENIED）。
    CancelledByUser,
    /// ShellExecute 本身失败，括号内是其返回值（≤32 的错误码）。
    Failed(i32),
}

/// ShellExecuteW 的返回值 → 提权结果。约定：返回值 > 32 为成功；
/// 5（SE_ERR_ACCESSDENIED）在 runas 场景下即用户取消 UAC。
pub fn outcome_from_shell_result(code: isize) -> ElevateOutcome {
    const SE_ERR_ACCESSDENIED: isize = 5;
    if code > 32 {
        ElevateOutcome::Launched
    } else if code == SE_ERR_ACCESSDENIED {
        ElevateOutcome::CancelledByUser
    } else {
        ElevateOutcome::Failed(code as i32)
    }
}

/// 提权尝试失败/取消后，失败页上给用户的中文提示。
pub fn note_for_outcome(outcome: ElevateOutcome) -> Option<String> {
    match outcome {
        ElevateOutcome::Launched => None,
        ElevateOutcome::CancelledByUser => Some(
            "刚才在系统弹窗里选了「否」，提权已取消。可以再点一次按钮，在弹窗里选「是」；\
             或者关掉本窗口，右键程序选择「以管理员身份运行」。"
                .to_owned(),
        ),
        ElevateOutcome::Failed(code) => Some(format!(
            "提权启动没有成功（系统返回代码 {code}）。请关掉本窗口，右键程序选择「以管理员身份运行」手动打开。"
        )),
    }
}

/// 以管理员身份重新拉起本程序（带 [`RELAUNCH_ARG`] 参数），
/// 触发系统 UAC 弹窗。仅 Windows 有实现；其他平台恒为失败
/// （失败页在其他平台不会被实际使用，仅为保持可编译）。
#[cfg(windows)]
pub fn relaunch_elevated() -> ElevateOutcome {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let Ok(exe) = std::env::current_exe() else {
        return ElevateOutcome::Failed(0);
    };
    let exe_wide: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb_wide: Vec<u16> = "runas\0".encode_utf16().collect();
    let params_wide: Vec<u16> = format!("{RELAUNCH_ARG}\0").encode_utf16().collect();
    // SAFETY: 三个字符串均为本函数内构造的 NUL 结尾 UTF-16 缓冲，
    // 在调用期间保持存活；句柄为空表示桌面窗口，符合 ShellExecute 约定。
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR(params_wide.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    outcome_from_shell_result(result.0 as isize)
}

/// 非 Windows 平台占位（开发/测试环境用）。
#[cfg(not(windows))]
pub fn relaunch_elevated() -> ElevateOutcome {
    ElevateOutcome::Failed(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_arg_detection() {
        assert!(has_relaunch_arg(["throttle.exe", RELAUNCH_ARG].into_iter()));
        assert!(!has_relaunch_arg(["throttle.exe"].into_iter()));
        assert!(!has_relaunch_arg(
            ["throttle.exe", "--headless"].into_iter()
        ));
        // 必须是完整参数相等，前缀相似不算。
        assert!(!has_relaunch_arg(
            ["throttle.exe", "--elevated-relaunch-x"].into_iter()
        ));
    }

    #[test]
    fn only_not_elevated_shows_elevate_button() {
        assert!(actions_for(FailureKind::NotElevated).show_elevate_button);
        assert!(!actions_for(FailureKind::MissingDriverFiles).show_elevate_button);
        assert!(!actions_for(FailureKind::Other).show_elevate_button);
    }

    #[test]
    fn not_elevated_body_explains_autostart_is_normal() {
        let body = actions_for(FailureKind::NotElevated).body_text;
        assert!(body.contains("开机自启动"));
        assert!(body.contains("正常现象"));
        assert!(body.contains("以管理员身份重新启动"));
    }

    #[test]
    fn missing_driver_body_names_the_file_and_skips_elevation() {
        let body = actions_for(FailureKind::MissingDriverFiles).body_text;
        assert!(body.contains("WinDivert64.sys"));
        assert!(body.contains("同一个目录"));
    }

    #[test]
    fn shell_result_mapping() {
        // >32 成功（含边界 33 与典型的大数值）。
        assert_eq!(outcome_from_shell_result(33), ElevateOutcome::Launched);
        assert_eq!(
            outcome_from_shell_result(0x1_0000),
            ElevateOutcome::Launched
        );
        // 5 = SE_ERR_ACCESSDENIED：UAC 点了「否」。
        assert_eq!(
            outcome_from_shell_result(5),
            ElevateOutcome::CancelledByUser
        );
        // 其余 ≤32 都是失败，保留原码：2 文件找不到、0 内存不足、32 边界。
        assert_eq!(outcome_from_shell_result(2), ElevateOutcome::Failed(2));
        assert_eq!(outcome_from_shell_result(0), ElevateOutcome::Failed(0));
        assert_eq!(outcome_from_shell_result(32), ElevateOutcome::Failed(32));
    }

    #[test]
    fn notes_cover_cancel_and_failure_but_not_success() {
        assert!(note_for_outcome(ElevateOutcome::Launched).is_none());
        let cancel = note_for_outcome(ElevateOutcome::CancelledByUser).unwrap();
        assert!(cancel.contains("选了「否」"));
        let failed = note_for_outcome(ElevateOutcome::Failed(2)).unwrap();
        assert!(failed.contains('2'));
        assert!(failed.contains("以管理员身份运行"));
    }
}
