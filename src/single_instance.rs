//! Single-instance guard.
//!
//! Two Throttle processes would each open a WinDivert handle at the same
//! priority, so every packet would be captured and re-injected twice: the
//! counters split the traffic between them and a limit set in one window can be
//! bypassed by the other. A machine-wide named mutex guarantees one shaper.
//! Because Throttle always runs elevated, the mutex lives in the `Global\`
//! namespace so instances in different sessions still see each other.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};
use windows::core::{PCWSTR, w};

/// Title of the main window, used to locate an already-running instance.
/// 注意：主窗口标题自中文化后是「流量监控」，必须与 main.rs 里
/// ViewportBuilder::with_title 的值保持一致，否则找不到已有窗口。
const WINDOW_TITLE: PCWSTR = w!("流量监控");

/// Held for the life of the process. Dropping it would release the mutex, so
/// the handle is intentionally never closed.
pub struct InstanceLock {
    #[allow(dead_code)]
    handle: HANDLE,
}

/// Try to become the single running instance.
///
/// Returns `Some(lock)` if this process now owns the mutex. Returns `None` if
/// another instance already holds it; in that case the existing main window,
/// if it can be found, has been restored and brought to the foreground so the
/// user's double-click still does something visible.
pub fn acquire() -> Option<InstanceLock> {
    match try_acquire() {
        AcquireAttempt::Acquired(lock) => Some(lock),
        AcquireAttempt::AlreadyRunning => {
            focus_existing_window();
            None
        }
        AcquireAttempt::Failed => None,
    }
}

/// 与 [`acquire`] 相同，但在锁被占用时先等待一段时间再放弃。
///
/// 只供失败页「以管理员身份重新启动」拉起的新实例使用：旧实例在拉起
/// 成功后立即退出、进程退出即释放互斥量，但新进程的启动与旧进程的
/// 退出存在竞态，新实例抢锁时旧锁可能还没释放。普通双击启动不走
/// 这里，行为与从前一致（立即唤起已有窗口）。
pub fn acquire_wait(timeout: std::time::Duration) -> Option<InstanceLock> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match try_acquire() {
            AcquireAttempt::Acquired(lock) => return Some(lock),
            AcquireAttempt::Failed => return None,
            AcquireAttempt::AlreadyRunning => {
                if std::time::Instant::now() >= deadline {
                    focus_existing_window();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
}

/// 一次抢锁尝试的结果。
enum AcquireAttempt {
    Acquired(InstanceLock),
    /// 锁已被另一实例持有（本次尝试的句柄已关闭）。
    AlreadyRunning,
    /// CreateMutexW 本身失败。
    Failed,
}

fn try_acquire() -> AcquireAttempt {
    // SAFETY: plain Win32 calls with valid, null-terminated static strings.
    unsafe {
        let handle = match CreateMutexW(None, false, w!("Global\\ThrottleSingleInstance")) {
            Ok(h) => h,
            Err(_) => return AcquireAttempt::Failed,
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // 没抢到：关掉本次句柄，锁仍由对方持有。
            let _ = CloseHandle(handle);
            return AcquireAttempt::AlreadyRunning;
        }
        AcquireAttempt::Acquired(InstanceLock { handle })
    }
}

/// Best-effort: restore and raise the existing instance's main window.
/// 已有实例可能处于三种状态：正常显示、最小化、或缩在系统托盘里
/// （窗口被隐藏）——隐藏状态先用 SW_SHOW 把窗口显出来再置前台；
/// GUI 侧还会凭焦点事件对齐它自己的隐藏标记。
fn focus_existing_window() {
    // SAFETY: FindWindowW returns a null HWND when nothing matches; all calls
    // tolerate that and return failure rather than faulting.
    unsafe {
        let hwnd: HWND = match FindWindowW(None, WINDOW_TITLE) {
            Ok(h) if !h.is_invalid() => h,
            _ => return,
        };
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else if !IsWindowVisible(hwnd).as_bool() {
            // 缩在托盘里的隐藏窗口：FindWindowW 能找到但不可见。
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}
