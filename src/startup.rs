//! 开机自启动管理：当前用户登录启动项（Windows `HKCU\...\Run`）。
//!
//! 设计要点：
//! - 只写当前用户（HKCU）的 `Software\Microsoft\Windows\CurrentVersion\Run`，
//!   值名 [`RUN_VALUE_NAME`]；不需要管理员权限，不写 HKLM、不建服务、
//!   不建计划任务。启动命令就是当前 exe 的完整路径（双引号包裹，无参数）。
//! - 状态判定不依赖任何本地缓存：每次都实时读注册表，再把存量命令里的
//!   路径与 `std::env::current_exe()` 比较；不一致即"已失效"（程序被移动
//!   或改名），界面据此提供"修复"（按当前路径重写）与"关闭"（删除）。
//! - 写入/删除后一律回读校验，校验不过就返回中文错误，绝不假报成功。
//! - 注册表访问抽象为 [`StartupStore`] trait：Windows 实现走 winreg
//!   （`#[cfg(windows)]`），单元测试用内存假实现，绝不触碰真实注册表。
//! - 本模块的逻辑部分仅依赖 std，可在任意平台独立编译与测试。

use std::io;
use std::path::Path;

/// Run 键内的值名。
pub const RUN_VALUE_NAME: &str = "Throttle";
/// HKCU 下 Run 键的路径（用于实现与界面说明）。
pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 面向用户的完整位置说明。
pub const RUN_KEY_DISPLAY: &str =
    r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run";

/// 注册表启动项的存储后端。实现必须保证 `delete` 幂等：
/// 启动项本就不存在时也视为成功。
pub trait StartupStore {
    /// 读取启动项的原始命令；不存在返回 `Ok(None)`。
    fn read(&self) -> io::Result<Option<String>>;
    /// 写入（覆盖）启动项命令。
    fn write(&self, command: &str) -> io::Result<()>;
    /// 删除启动项；不存在也算成功。
    fn delete(&self) -> io::Result<()>;
}

/// 开机自启动的真实状态（每次由实时读取注册表得出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupStatus {
    /// 未启用：没有启动项。
    Disabled,
    /// 已启用：启动项指向当前 exe。
    Enabled,
    /// 已失效：启动项还在，但指向的不是当前 exe（程序被移动或改名）。
    Stale {
        /// 注册表中存量命令的原文（供界面展示与解释）。
        stored_command: String,
    },
    /// 读取失败（括号内为失败原因，界面直接展示）。
    ReadError(String),
}

impl StartupStatus {
    /// 一行中文状态说明（界面"当前状态"行）。
    pub fn describe(&self) -> String {
        match self {
            StartupStatus::Disabled => "未启用".to_string(),
            StartupStatus::Enabled => "已启用（登录 Windows 时自动启动）".to_string(),
            StartupStatus::Stale { .. } => "已失效（启动项指向的程序已被移动或改名）".to_string(),
            StartupStatus::ReadError(e) => format!("读取失败：{e}"),
        }
    }
}

/// 由 exe 完整路径拼出启动命令：双引号包裹、无任何参数。
pub fn build_command(exe_path: &Path) -> String {
    format!("\"{}\"", exe_path.to_string_lossy())
}

/// 从存量启动命令中解析出程序路径。
///
/// 支持 `"C:\a b\app.exe"`（可带后续参数）与 `C:\a b\app.exe` 两种写法；
/// 未加引号且带参数时按第一个 `.exe` 结尾处截断。解析不出返回 `None`。
pub fn parse_command_path(command: &str) -> Option<String> {
    let s = command.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('"') {
        return match rest.find('"') {
            Some(end) => Some(rest[..end].to_string()),
            None => Some(rest.to_string()),
        };
    }
    let lower = s.to_lowercase();
    if let Some(idx) = lower.find(".exe") {
        return Some(s[..idx + 4].to_string());
    }
    Some(s.to_string())
}

/// 路径规范化：去引号、统一分隔符、去尾随反斜杠、转小写。
fn normalize_path(p: &str) -> String {
    let mut s = p.trim().to_string();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_string();
    }
    s = s.replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s.to_lowercase()
}

/// 两个路径是否指向同一程序（忽略引号、大小写与分隔符差异）。
pub fn paths_equal(a: &str, b: &str) -> bool {
    normalize_path(a) == normalize_path(b)
}

/// 实时读取注册表并判定当前状态。
pub fn current_status<S: StartupStore>(store: &S, current_exe: &Path) -> StartupStatus {
    match store.read() {
        Err(e) => StartupStatus::ReadError(e.to_string()),
        Ok(None) => StartupStatus::Disabled,
        Ok(Some(command)) => {
            let exe = current_exe.to_string_lossy();
            match parse_command_path(&command) {
                Some(stored) if paths_equal(&stored, &exe) => StartupStatus::Enabled,
                _ => StartupStatus::Stale {
                    stored_command: command,
                },
            }
        }
    }
}

/// 启用开机自启动：按当前 exe 路径写入启动项，写后回读逐字校验。
/// 成功返回最新状态；失败返回中文错误说明（状态以注册表实际为准）。
pub fn enable<S: StartupStore>(store: &S, current_exe: &Path) -> Result<StartupStatus, String> {
    let command = build_command(current_exe);
    store
        .write(&command)
        .map_err(|e| format!("写入开机启动项失败：{e}"))?;
    match store.read() {
        Ok(Some(back)) if back == command => Ok(StartupStatus::Enabled),
        Ok(Some(back)) => Err(format!(
            "写入后校验不一致：注册表中的启动命令是「{back}」，预期是「{command}」，请重试"
        )),
        Ok(None) => Err("写入后校验失败：启动项不存在，写入可能未生效".to_string()),
        Err(e) => Err(format!("写入后回读启动项失败：{e}")),
    }
}

/// 关闭开机自启动：删除启动项（不存在也算成功），删后回读确认已不存在。
pub fn disable<S: StartupStore>(store: &S) -> Result<StartupStatus, String> {
    store
        .delete()
        .map_err(|e| format!("删除开机启动项失败：{e}"))?;
    match store.read() {
        Ok(None) => Ok(StartupStatus::Disabled),
        Ok(Some(back)) => Err(format!(
            "删除后校验失败：启动项仍然存在（「{back}」），删除可能未生效"
        )),
        Err(e) => Err(format!("删除后回读启动项失败：{e}")),
    }
}

// ---------------------------------------------------------------------------
// Windows 实现：HKCU Run 键（winreg）
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub struct RegistryStartupStore;

#[cfg(windows)]
impl StartupStore for RegistryStartupStore {
    fn read(&self) -> io::Result<Option<String>> {
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey(RUN_KEY_PATH) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        match key.get_value::<String, _>(RUN_VALUE_NAME) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, command: &str) -> io::Result<()> {
        use winreg::enums::HKEY_CURRENT_USER;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        // create_subkey：键不存在时创建，存在时打开（带写权限）。
        let (key, _) = hkcu.create_subkey(RUN_KEY_PATH)?;
        key.set_value(RUN_VALUE_NAME, &command)
    }

    fn delete(&self) -> io::Result<()> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey_with_flags(RUN_KEY_PATH, KEY_QUERY_VALUE | KEY_SET_VALUE) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match key.delete_value(RUN_VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// 测试：全部走内存假实现，绝不触碰真实注册表。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    /// 内存假注册表：可注入读/写/删失败，并可模拟"写入后回读不一致"。
    #[derive(Default)]
    struct FakeStore {
        value: RefCell<Option<String>>,
        fail_read: Cell<bool>,
        fail_write: Cell<bool>,
        fail_delete: Cell<bool>,
        /// 若设置，write 时实际存入这个值（模拟被别处改写/写坏）。
        write_override: RefCell<Option<String>>,
    }

    impl StartupStore for FakeStore {
        fn read(&self) -> io::Result<Option<String>> {
            if self.fail_read.get() {
                return Err(io::Error::other("模拟读取失败"));
            }
            Ok(self.value.borrow().clone())
        }
        fn write(&self, command: &str) -> io::Result<()> {
            if self.fail_write.get() {
                return Err(io::Error::other("模拟写入失败"));
            }
            let stored = self
                .write_override
                .borrow()
                .clone()
                .unwrap_or_else(|| command.to_string());
            *self.value.borrow_mut() = Some(stored);
            Ok(())
        }
        fn delete(&self) -> io::Result<()> {
            if self.fail_delete.get() {
                return Err(io::Error::other("模拟删除失败"));
            }
            *self.value.borrow_mut() = None;
            Ok(())
        }
    }

    fn exe(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    #[test]
    fn enable_writes_quoted_full_command() {
        let store = FakeStore::default();
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        let st = enable(&store, &path).expect("启用应成功");
        assert_eq!(st, StartupStatus::Enabled);
        assert_eq!(
            store.value.borrow().as_deref(),
            Some(r#""C:\Tools\Throttle\throttle.exe""#)
        );
        // 写入后状态判定为已启用。
        assert_eq!(current_status(&store, &path), StartupStatus::Enabled);
    }

    #[test]
    fn enable_quotes_path_with_spaces() {
        let store = FakeStore::default();
        let path = exe(r"C:\Program Files\Throttle\throttle.exe");
        enable(&store, &path).expect("启用应成功");
        assert_eq!(
            store.value.borrow().as_deref(),
            Some(r#""C:\Program Files\Throttle\throttle.exe""#),
            "含空格路径必须被双引号包裹"
        );
    }

    #[test]
    fn disable_deletes_value() {
        let store = FakeStore::default();
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        enable(&store, &path).unwrap();
        let st = disable(&store).expect("关闭应成功");
        assert_eq!(st, StartupStatus::Disabled);
        assert_eq!(store.value.borrow().as_deref(), None);
        assert_eq!(current_status(&store, &path), StartupStatus::Disabled);
    }

    #[test]
    fn disable_when_absent_is_ok() {
        let store = FakeStore::default();
        let st = disable(&store).expect("启动项不存在时关闭也应成功");
        assert_eq!(st, StartupStatus::Disabled);
    }

    #[test]
    fn parse_quoted_command() {
        assert_eq!(
            parse_command_path(r#""C:\a b\throttle.exe""#).as_deref(),
            Some(r"C:\a b\throttle.exe")
        );
        // 带引号且后面跟参数：只取路径部分。
        assert_eq!(
            parse_command_path(r#""C:\Tools\throttle.exe" /silent"#).as_deref(),
            Some(r"C:\Tools\throttle.exe")
        );
    }

    #[test]
    fn parse_unquoted_command() {
        assert_eq!(
            parse_command_path(r"C:\Tools\throttle.exe").as_deref(),
            Some(r"C:\Tools\throttle.exe")
        );
        // 未加引号且带参数：按 .exe 结尾截断。
        assert_eq!(
            parse_command_path(r"C:\a b\throttle.exe --flag").as_deref(),
            Some(r"C:\a b\throttle.exe")
        );
        assert_eq!(parse_command_path("   "), None);
    }

    #[test]
    fn stale_when_path_differs() {
        let store = FakeStore::default();
        *store.value.borrow_mut() = Some(r#""D:\old\throttle.exe""#.to_string());
        let cur = exe(r"C:\new\throttle.exe");
        match current_status(&store, &cur) {
            StartupStatus::Stale { stored_command } => {
                assert_eq!(stored_command, r#""D:\old\throttle.exe""#);
            }
            other => panic!("应判定为已失效，实际为 {other:?}"),
        }
    }

    #[test]
    fn enabled_despite_case_and_quote_differences() {
        let store = FakeStore::default();
        *store.value.borrow_mut() = Some(r"c:\program files\throttle\throttle.exe".to_string());
        let cur = exe(r"C:\Program Files\Throttle\throttle.exe");
        assert_eq!(current_status(&store, &cur), StartupStatus::Enabled);
    }

    #[test]
    fn write_failure_propagates_and_state_unchanged() {
        let store = FakeStore::default();
        store.fail_write.set(true);
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        let err = enable(&store, &path).expect_err("写入失败必须报错");
        assert!(
            err.contains("写入开机启动项失败"),
            "错误应为中文说明：{err}"
        );
        // 状态不能被假报为成功：注册表仍为空。
        assert_eq!(current_status(&store, &path), StartupStatus::Disabled);
    }

    #[test]
    fn readback_mismatch_is_error() {
        let store = FakeStore::default();
        *store.write_override.borrow_mut() = Some(r#""C:\somewhere\else.exe""#.to_string());
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        let err = enable(&store, &path).expect_err("回读不一致必须报错");
        assert!(err.contains("校验不一致"), "错误应说明校验不一致：{err}");
    }

    #[test]
    fn read_failure_reports_error_status() {
        let store = FakeStore::default();
        store.fail_read.set(true);
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        match current_status(&store, &path) {
            StartupStatus::ReadError(msg) => assert!(msg.contains("模拟读取失败")),
            other => panic!("读取失败应返回 ReadError，实际为 {other:?}"),
        }
    }

    #[test]
    fn delete_failure_propagates() {
        let store = FakeStore::default();
        let path = exe(r"C:\Tools\Throttle\throttle.exe");
        enable(&store, &path).unwrap();
        store.fail_delete.set(true);
        let err = disable(&store).expect_err("删除失败必须报错");
        assert!(
            err.contains("删除开机启动项失败"),
            "错误应为中文说明：{err}"
        );
        // 启动项还在，状态仍为已启用。
        assert_eq!(current_status(&store, &path), StartupStatus::Enabled);
    }

    #[test]
    fn repair_overwrites_stale_entry() {
        let store = FakeStore::default();
        *store.value.borrow_mut() = Some(r#""D:\old\throttle.exe""#.to_string());
        let cur = exe(r"C:\new\throttle.exe");
        // "修复"即按当前路径重新启用。
        let st = enable(&store, &cur).expect("修复应成功");
        assert_eq!(st, StartupStatus::Enabled);
        assert_eq!(current_status(&store, &cur), StartupStatus::Enabled);
    }
}
