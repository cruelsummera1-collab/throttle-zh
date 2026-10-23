//! 系统弹窗通知（Windows Toast）：告警的第三条提醒通道。
//!
//! 与窗口顶部横幅、历史记录并列：后端告警状态机每次**触发**一个告警事件
//! （同一段持续异常只产生一个事件，回落到阈值 80% 以下重新布防后再次
//! 触发才产生下一个）时，调用 [`notify_alert`] 弹一条 Windows 系统通知。
//! 程序启动时**不**补弹历史告警。
//!
//! 设计要点：
//! - 文案拼装（[`toast_text`]）是纯函数，仅依赖 std，任意平台可测；
//!   字段与数值全部来自触发事件本身（进程名/PID/上传速率/已持续秒数/阈值），
//!   速率格式与界面横幅一致（1024 进制 [`humanize_rate`]）。
//! - 发送在后端线程之外的一次性短线程里完成，绝不阻塞每秒的聚合循环；
//!   全路径失败只记一条日志（`tracing::warn`），不弹窗报错、不 panic。
//! - 发送实现（`#[cfg(windows)]`）：crate `winrt-toast` 调
//!   `ToastNotificationManager`；未打包的 Win32 程序要让 WinRT 通知真正
//!   显示，需要进程 AppUserModelID 与开始菜单中带相同 AUMID 属性的快捷
//!   方式，[`init`] 在启动时设置进程 AUMID 并在后台线程检查/补建快捷方式
//!   （指向当前 exe）。这两步失败同样只记日志、静默降级——横幅与历史
//!   两条通道不受影响。
//! - 非 Windows 平台：本模块只剩纯函数与空操作入口，保持可编译。

/// 通知标题（与开始菜单快捷方式名、窗口标题保持一致的品牌前缀）。
pub const TOAST_TITLE: &str = "流量监控：异常上传提醒";

/// 本程序的 AppUserModelID：进程、开始菜单快捷方式、Toast 三处必须一致。
#[cfg(windows)]
pub const APP_USER_MODEL_ID: &str = "Throttle.Zh.Monitor";

/// 开始菜单快捷方式的显示名（通知在系统通知设置中的条目名来自于此）。
#[cfg(windows)]
pub const SHORTCUT_DISPLAY_NAME: &str = "流量监控";

const KB: f64 = 1024.0;
const MB: f64 = 1024.0 * 1024.0;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// 速率格式化：1024 进制，与界面（gui.rs 的 humanize_rate）保持一致。
pub fn humanize_rate(bps: u64) -> String {
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

/// 由告警事件字段拼通知的（标题，正文）。纯函数。
///
/// 正文结构与后端日志行一致：进程名、PID、上传速率、已持续秒数、阈值。
pub fn toast_text(
    name: &str,
    pid: u32,
    up_rate: u64,
    sustained_secs: u64,
    threshold_bps: u64,
) -> (String, String) {
    let body = format!(
        "{}（PID {}）正在以 {} 上传，已持续 {} 秒（阈值 {}）",
        name,
        pid,
        humanize_rate(up_rate),
        sustained_secs,
        humanize_rate(threshold_bps),
    );
    (TOAST_TITLE.to_string(), body)
}

/// XML 转义。当前发送走 winrt-toast（由其内部渲染 XML），本函数与
/// [`toast_xml`] 是直接调 windows crate ToastNotificationManager 时的
/// 备用拼装路径，作为纯函数保留并有单元测试覆盖，故允许 dead_code。
#[allow(dead_code)]
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// 拼出 ToastGeneric 模板的通知 XML（标题一行、正文一行）。定位同
/// [`xml_escape`]：备用拼装路径，保留并有测试覆盖。
#[allow(dead_code)]
pub fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body),
    )
}

/// 后端在告警事件触发时调用：弹一条系统通知。
///
/// 每个事件只调用一次（事件本身由状态机保证每段异常只产生一个）。
/// 非 Windows 平台为空操作。
pub fn notify_alert(name: &str, pid: u32, up_rate: u64, sustained_secs: u64, threshold_bps: u64) {
    #[cfg(windows)]
    {
        let (title, body) = toast_text(name, pid, up_rate, sustained_secs, threshold_bps);
        let spawned = std::thread::Builder::new()
            .name("alert-toast".to_string())
            .spawn(move || {
                if let Err(e) = win::show_toast(&title, &body) {
                    tracing::warn!("系统弹窗通知发送失败（不影响监控与历史记录）：{e}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("系统弹窗通知线程创建失败（不影响监控与历史记录）：{e}");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (name, pid, up_rate, sustained_secs, threshold_bps);
    }
}

/// 发送一条通用系统通知（与告警弹窗同一通道、同一发送方式）：
/// 目前用于「首次缩到系统托盘」的提示。失败只记日志。
/// 非 Windows 平台为空操作。
pub fn notify_message(title: &str, body: &str) {
    #[cfg(windows)]
    {
        let title = title.to_string();
        let body = body.to_string();
        let spawned = std::thread::Builder::new()
            .name("message-toast".to_string())
            .spawn(move || {
                if let Err(e) = win::show_toast(&title, &body) {
                    tracing::warn!("系统通知发送失败（不影响主流程）：{e}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("系统通知线程创建失败（不影响主流程）：{e}");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body);
    }
}

/// 程序启动时调用一次：为系统通知做准备（设置进程 AUMID、检查/补建
/// 开始菜单快捷方式）。失败只记日志，绝不影响主流程。
/// 非 Windows 平台为空操作。
pub fn init() {
    #[cfg(windows)]
    win::init();
}

#[cfg(windows)]
mod win {
    use super::{APP_USER_MODEL_ID, SHORTCUT_DISPLAY_NAME};
    use std::path::{Path, PathBuf};
    use windows::core::{Interface, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, STGM_READ,
    };
    use windows::Win32::System::Variant::{VT_EMPTY, VT_LPWSTR};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink,
    };

    /// UTF-16 NUL 结尾缓冲（供 PCWSTR 参数使用）。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// COM 初始化守卫：RPC_E_CHANGED_MODE（线程已用别的套间模型初始化）
    /// 视为可用但不负责反初始化；S_FALSE（重复初始化）仍需配平。
    struct CoInit(bool);

    impl CoInit {
        fn new(model: COINIT) -> Result<Self, String> {
            let hr = unsafe { CoInitializeEx(None, model) };
            if hr.is_ok() {
                Ok(CoInit(true))
            } else if hr == RPC_E_CHANGED_MODE {
                Ok(CoInit(false))
            } else {
                Err(format!("CoInitializeEx 失败（HRESULT {hr:?}）"))
            }
        }
    }

    impl Drop for CoInit {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() }
            }
        }
    }

    /// 启动准备：设置进程 AUMID（主线程，廉价）；快捷方式检查放到后台
    /// 线程（涉及 COM 与文件读写），不拖慢启动。
    pub fn init() {
        let aumid = wide(APP_USER_MODEL_ID);
        if let Err(e) =
            unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr())) }
        {
            tracing::warn!("设置进程 AppUserModelID 失败（系统通知可能不显示）：{e}");
        }
        let spawned = std::thread::Builder::new()
            .name("toast-shortcut".to_string())
            .spawn(|| {
                if let Err(e) = ensure_start_menu_shortcut() {
                    tracing::warn!("开始菜单快捷方式准备失败（系统通知可能不显示）：{e}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("开始菜单快捷方式准备线程创建失败（系统通知可能不显示）：{e}");
        }
    }

    /// 发送一条 Toast 通知（winrt-toast → ToastNotificationManager）。
    pub fn show_toast(title: &str, body: &str) -> Result<(), String> {
        let _co = CoInit::new(COINIT_MULTITHREADED)?;
        let manager = winrt_toast::ToastManager::new(APP_USER_MODEL_ID);
        let mut toast = winrt_toast::Toast::new();
        toast.text1(title).text2(body);
        manager
            .show(&toast)
            .map_err(|e| format!("Toast 显示失败：{e}"))
    }

    /// 开始菜单快捷方式路径：%APPDATA%\Microsoft\Windows\Start Menu\Programs\流量监控.lnk
    fn shortcut_path() -> Result<PathBuf, String> {
        let appdata = std::env::var_os("APPDATA")
            .ok_or_else(|| "无法读取 APPDATA 环境变量".to_string())?;
        Ok(PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join(format!("{SHORTCUT_DISPLAY_NAME}.lnk")))
    }

    /// PKEY_AppUserModel_ID（{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5）。
    fn aumid_property_key() -> PROPERTYKEY {
        PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        }
    }

    /// 快捷方式不存在、或指向的不是当前 exe、或 AUMID 不对时（重）建之；
    /// 已正确指向当前程序则什么都不做（幂等，移动程序后下次启动自动修复）。
    fn ensure_start_menu_shortcut() -> Result<(), String> {
        let exe =
            std::env::current_exe().map_err(|e| format!("无法确定当前程序路径：{e}"))?;
        let lnk = shortcut_path()?;
        let _co = CoInit::new(COINIT_APARTMENTTHREADED)?;
        if lnk.exists() && shortcut_is_current(&lnk, &exe) {
            return Ok(());
        }
        create_shortcut(&lnk, &exe)
    }

    /// 读已有快捷方式：目标 == 当前 exe 且 AUMID 属性 == 本程序 AUMID。
    /// 任一步失败都按"不符合"处理（调用方会重建）。
    fn shortcut_is_current(lnk: &Path, exe: &Path) -> bool {
        unsafe {
            let Ok(link): Result<IShellLinkW, _> =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            else {
                return false;
            };
            let Ok(persist) = link.cast::<IPersistFile>() else {
                return false;
            };
            let lnk_w = wide(&lnk.to_string_lossy());
            if persist.Load(PCWSTR(lnk_w.as_ptr()), STGM_READ).is_err() {
                return false;
            }
            let mut buf = [0u16; 1024];
            if link.GetPath(&mut buf, core::ptr::null_mut(), 0).is_err() {
                return false;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let target = String::from_utf16_lossy(&buf[..end]);
            if !target.eq_ignore_ascii_case(&exe.to_string_lossy()) {
                return false;
            }
            let Ok(store) = link.cast::<IPropertyStore>() else {
                return false;
            };
            let key = aumid_property_key();
            let Ok(mut pv) = store.GetValue(&key) else {
                return false;
            };
            let ok = pv.Anonymous.Anonymous.vt == VT_LPWSTR && {
                let p = pv.Anonymous.Anonymous.Anonymous.pwszVal;
                !p.is_null()
                    && read_pwstr(p).is_some_and(|s| s == APP_USER_MODEL_ID)
            };
            let _ = windows::Win32::System::Com::StructuredStorage::PropVariantClear(
                &mut pv,
            );
            ok
        }
    }

    /// 读 NUL 结尾的 UTF-16 字符串（PWSTR 指向的内存由调用方保证有效）。
    unsafe fn read_pwstr(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while unsafe { *p.0.add(len) } != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(unsafe {
            core::slice::from_raw_parts(p.0, len)
        }))
    }

    /// 创建/覆盖快捷方式：目标=当前 exe、工作目录=exe 所在目录、图标=exe，
    /// 并写入 AUMID 属性后落盘。
    fn create_shortcut(lnk: &Path, exe: &Path) -> Result<(), String> {
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("创建 ShellLink 失败：{e}"))?;
            let exe_s = exe.to_string_lossy().to_string();
            let exe_w = wide(&exe_s);
            link.SetPath(PCWSTR(exe_w.as_ptr()))
                .map_err(|e| format!("设置快捷方式目标失败：{e}"))?;
            if let Some(parent) = exe.parent() {
                let parent_w = wide(&parent.to_string_lossy());
                let _ = link.SetWorkingDirectory(PCWSTR(parent_w.as_ptr()));
            }
            let desc_w = wide(SHORTCUT_DISPLAY_NAME);
            let _ = link.SetDescription(PCWSTR(desc_w.as_ptr()));
            let _ = link.SetIconLocation(PCWSTR(exe_w.as_ptr()), 0);

            // AUMID 属性：PROPVARIANT 手工构造（VT_LPWSTR 指向自有缓冲，
            // SetValue 会复制字符串）；Commit 后把 vt 清空并 forget，
            // 避免任何按 CoTaskMem 释放自有缓冲的企图。
            let store: IPropertyStore = link
                .cast()
                .map_err(|e| format!("获取快捷方式属性存储失败：{e}"))?;
            let mut aumid_buf = wide(APP_USER_MODEL_ID);
            let mut pv = PROPVARIANT::default();
            (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
            (*pv.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(aumid_buf.as_mut_ptr());
            let key = aumid_property_key();
            let set_result = store.SetValue(&key, &pv);
            (*pv.Anonymous.Anonymous).vt = VT_EMPTY;
            core::mem::forget(pv);
            set_result.map_err(|e| format!("写入快捷方式 AUMID 失败：{e}"))?;
            store
                .Commit()
                .map_err(|e| format!("提交快捷方式属性失败：{e}"))?;

            if let Some(dir) = lnk.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let persist: IPersistFile = link
                .cast()
                .map_err(|e| format!("获取快捷方式保存接口失败：{e}"))?;
            let lnk_w = wide(&lnk.to_string_lossy());
            persist
                .Save(PCWSTR(lnk_w.as_ptr()), true)
                .map_err(|e| format!("保存快捷方式失败：{e}"))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_uses_event_fields() {
        let (title, body) = toast_text("chrome.exe", 4321, 512 * 1024, 30, 500 * 1024);
        assert_eq!(title, "流量监控：异常上传提醒");
        assert!(body.contains("chrome.exe"), "正文应含进程名：{body}");
        assert!(body.contains("4321"), "正文应含 PID：{body}");
        assert!(body.contains("512.0 KB/s"), "正文应含当前速率：{body}");
        assert!(body.contains("30 秒"), "正文应含已持续秒数：{body}");
        assert!(body.contains("500.0 KB/s"), "正文应含阈值：{body}");
    }

    #[test]
    fn toast_text_small_rates_stay_in_bytes() {
        let (_, body) = toast_text("a.exe", 1, 300, 45, 100);
        assert!(body.contains("300 B/s"), "{body}");
        assert!(body.contains("100 B/s"), "{body}");
        assert!(body.contains("45 秒"), "{body}");
    }

    #[test]
    fn humanize_rate_boundaries() {
        assert_eq!(humanize_rate(0), "0 B/s");
        assert_eq!(humanize_rate(1023), "1023 B/s");
        assert_eq!(humanize_rate(1024), "1.0 KB/s");
        assert_eq!(humanize_rate(500 * 1024), "500.0 KB/s");
        assert_eq!(humanize_rate(1024 * 1024), "1.00 MB/s");
        assert_eq!(humanize_rate(1024 * 1024 * 1024), "1.00 GB/s");
    }

    #[test]
    fn xml_escape_covers_special_chars() {
        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(xml_escape("普通文本"), "普通文本");
    }

    #[test]
    fn toast_xml_wraps_and_escapes() {
        let xml = toast_xml("标题<&", "正文&内容");
        assert!(xml.contains("template=\"ToastGeneric\""), "{xml}");
        assert!(xml.contains("<text>标题&lt;&amp;</text>"), "{xml}");
        assert!(xml.contains("<text>正文&amp;内容</text>"), "{xml}");
    }
}
