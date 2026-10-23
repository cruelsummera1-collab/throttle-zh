//! 进程中文友好显示名解析：只决定"显示成什么"，不参与任何内部身份。
//!
//! 规则、告警状态机、历史存储的键与字段始终用 exe 文件名与完整路径；
//! 本模块在展示层把它们翻译成用户更容易认的名字，优先级：
//!   1. 内置别名表（见 [`ALIASES`]，小而精，覆盖常见国内应用）；
//!   2. exe 文件 VERSIONINFO 元数据：FileDescription 优先，其次
//!      ProductName（值必须可读，纯版本号之类的无意义值不用）；
//!   3. 回退为 exe 文件名本身。
//!
//! 解析结果按 exe 完整路径缓存（成功与失败都缓存），同一路径只读一次
//! 元数据；读取失败/无权限/文件已消失一律回退 exe 名并只记一条日志，
//! 绝不 panic。名称选择与拼装是平台无关纯函数 + 一个可注入的元数据
//! 来源（[`MetadataProvider`]）；真正的 Windows 版本信息读取在
//! `#[cfg(windows)]` 内，非 Windows 平台不读元数据、回退 exe 名。

use std::collections::HashMap;
use std::sync::Mutex;

/// exe 文件 VERSIONINFO 里取出的原始字符串（未做可用性判断）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawMetadata {
    pub file_description: Option<String>,
    pub product_name: Option<String>,
}

/// 元数据来源的注入点：生产实现读真实文件的 VERSIONINFO，
/// 测试用计数假实现验证缓存行为。
pub trait MetadataProvider: Send + Sync {
    /// 读指定 exe 的原始元数据；读不到（失败/无版本信息）返回 None。
    fn raw_metadata(&self, exe_path: &str) -> Option<RawMetadata>;
}

/// 内置别名表：（小写 exe 文件名，中文显示名）。
///
/// 只收录名称无歧义、国内常见的应用；系统进程与小众程序不在表内，
/// 它们走元数据/文件名回退（系统进程的 FileDescription 多为英文，
/// 届时仍显示英文，属预期）。
pub static ALIASES: &[(&str, &str)] = &[
    ("weixin.exe", "微信"),
    ("wechatappex.exe", "微信小程序"),
    ("qq.exe", "QQ"),
    ("qqmusic.exe", "QQ音乐"),
    ("kugou.exe", "酷狗音乐"),
    ("kwmusic.exe", "酷我音乐"),
    ("cloudmusic.exe", "网易云音乐"),
    ("qyclient.exe", "爱奇艺"),
    ("qqlive.exe", "腾讯视频"),
    ("youku.exe", "优酷"),
    ("bilibili.exe", "哔哩哔哩"),
    ("douyin.exe", "抖音"),
    ("kuaishou.exe", "快手"),
    ("dingtalk.exe", "钉钉"),
    ("feishu.exe", "飞书"),
    ("wxwork.exe", "企业微信"),
    ("wemeetapp.exe", "腾讯会议"),
    ("baidunetdisk.exe", "百度网盘"),
    ("adrive.exe", "阿里云盘"),
    ("thunder.exe", "迅雷"),
    ("quark.exe", "夸克"),
    ("chrome.exe", "Chrome 浏览器"),
    ("msedge.exe", "Edge 浏览器"),
    ("firefox.exe", "Firefox 浏览器"),
    ("360se.exe", "360安全浏览器"),
    ("360chrome.exe", "360极速浏览器"),
    ("sogouexplorer.exe", "搜狗浏览器"),
    ("epicgameslauncher.exe", "Epic 启动器"),
    ("verge-mihomo.exe", "Clash Verge"),
    ("clash-verge.exe", "Clash Verge"),
];

/// 别名表查询：按 exe 文件名匹配，不区分大小写。
pub fn alias_for(exe_file_name: &str) -> Option<&'static str> {
    let lower = exe_file_name.to_lowercase();
    ALIASES
        .iter()
        .find(|(key, _)| *key == lower)
        .map(|(_, friendly)| *friendly)
}

/// 元数据字符串是否可用作显示名：非空、无乱码/控制字符，
/// 且不是纯版本号（如 "1.2.3.4"、"v2.0"）这类无意义值。
pub fn is_usable_metadata(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    if t.chars().any(|c| c == '\u{fffd}' || c.is_control()) {
        return false;
    }
    // 纯版本号判定：去掉可选的 v/V 前缀后只剩数字与点。
    let digits = t.strip_prefix(['v', 'V']).unwrap_or(t);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return false;
    }
    true
}

/// 在 exe 名与原始元数据之间做最终选择（纯函数）。
pub fn choose_display_name(exe_name: &str, meta: Option<&RawMetadata>) -> String {
    if let Some(friendly) = alias_for(exe_name) {
        return friendly.to_string();
    }
    if let Some(m) = meta {
        if let Some(fd) = m.file_description.as_deref().filter(|s| is_usable_metadata(s)) {
            return fd.trim().to_string();
        }
        if let Some(pn) = m.product_name.as_deref().filter(|s| is_usable_metadata(s)) {
            return pn.trim().to_string();
        }
    }
    exe_name.to_string()
}

/// 拼「友好名（exe名）」形式的辨识标签（横幅/弹窗/历史告警用）：
/// 两者相同（不区分大小写）时只显示一个。
pub fn format_label(friendly: &str, exe_name: &str) -> String {
    if friendly.is_empty() {
        return exe_name.to_string();
    }
    if friendly.eq_ignore_ascii_case(exe_name) {
        friendly.to_string()
    } else {
        format!("{friendly}（{exe_name}）")
    }
}

/// 带缓存的名字解析器：按 exe 完整路径（小写）缓存解析结果，
/// 失败结果同样缓存，避免对已消失/无权限的文件反复读取。
pub struct NameResolver<P: MetadataProvider> {
    provider: P,
    cache: Mutex<HashMap<String, String>>,
}

impl<P: MetadataProvider> NameResolver<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 解析显示名（单个名字的形式，表格主列/详情用）。
    pub fn display_name(&self, exe_path: &str, exe_name: &str) -> String {
        // 合成行（内核 System / 未归属流量）的键不是文件路径，
        // 直接给与界面一致的中文名，不查元数据。
        match exe_path {
            "system" => return "系统".to_string(),
            "<unknown>" => return "未归属流量".to_string(),
            _ => {}
        }
        let key = if exe_path.is_empty() {
            exe_name.to_lowercase()
        } else {
            exe_path.to_lowercase()
        };
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return hit.clone();
        }
        // 只有像文件路径的键才读元数据；其余（空路径等）直接走选择逻辑。
        let meta = if exe_path.contains(['\\', '/']) {
            self.provider.raw_metadata(exe_path)
        } else {
            None
        };
        let chosen = choose_display_name(exe_name, meta.as_ref());
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // 安全阀：条目数按不同 exe 路径的基数自然有界，但极端情况下
        // （大量短命程序的不同路径）也不无限增长；清掉后按需重新
        // 解析，结果与缓存命中完全一致。
        const MAX_CACHE_ENTRIES: usize = 1024;
        if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, chosen.clone());
        chosen
    }

    /// 解析辨识标签（「友好名（exe名）」形式，横幅/弹窗/历史告警用）。
    /// 合成行只显示中文名本身，不拼 exe 后缀。
    pub fn label(&self, exe_path: &str, exe_name: &str) -> String {
        match exe_path {
            "system" => return "系统".to_string(),
            "<unknown>" => return "未归属流量".to_string(),
            _ => {}
        }
        let friendly = self.display_name(exe_path, exe_name);
        format_label(&friendly, exe_name)
    }
}

/// 生产用元数据来源：Windows 读 VERSIONINFO，其余平台恒为 None。
pub struct PlatformProvider;

impl MetadataProvider for PlatformProvider {
    fn raw_metadata(&self, exe_path: &str) -> Option<RawMetadata> {
        #[cfg(windows)]
        {
            win::read_raw_metadata(exe_path)
        }
        #[cfg(not(windows))]
        {
            let _ = exe_path;
            None
        }
    }
}

fn global_resolver() -> &'static NameResolver<PlatformProvider> {
    static RESOLVER: std::sync::OnceLock<NameResolver<PlatformProvider>> =
        std::sync::OnceLock::new();
    RESOLVER.get_or_init(|| NameResolver::new(PlatformProvider))
}

/// 全局入口：进程的友好显示名（带缓存）。
pub fn display_name(exe_path: &str, exe_name: &str) -> String {
    global_resolver().display_name(exe_path, exe_name)
}

/// 全局入口：「友好名（exe名）」辨识标签（带缓存）。
pub fn process_label(exe_path: &str, exe_name: &str) -> String {
    global_resolver().label(exe_path, exe_name)
}

#[cfg(windows)]
mod win {
    use super::RawMetadata;
    use std::ffi::c_void;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    /// 读版本信息；全路径失败只记一条日志并回退（同一路径只解析一次，
    /// 失败结果由上层缓存，故每路径至多记一条）。
    pub fn read_raw_metadata(exe_path: &str) -> Option<RawMetadata> {
        match read_inner(exe_path) {
            Ok(meta) => meta,
            Err(e) => {
                tracing::warn!("读取进程友好名元数据失败（{exe_path}）：{e}");
                None
            }
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_inner(exe_path: &str) -> Result<Option<RawMetadata>, String> {
        let path_w = wide(exe_path);
        let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path_w.as_ptr()), None) };
        if size == 0 {
            return Ok(None);
        }
        let mut buf = vec![0u8; size as usize];
        unsafe {
            GetFileVersionInfoW(
                PCWSTR(path_w.as_ptr()),
                None,
                size,
                buf.as_mut_ptr() as *mut c_void,
            )
            .map_err(|e| format!("GetFileVersionInfoW 失败：{e}"))?;
        }
        // 语言顺序：文件自带的翻译列表在前，但把简体中文（0804）提到
        // 最前优先尝试；列表为空时兜底试简中/美英/语言中立的 Unicode 页。
        let mut translations = query_translations(&buf).unwrap_or_default();
        translations.sort_by_key(|(lang, _)| u8::from(*lang != 0x0804));
        if translations.is_empty() {
            translations = vec![(0x0804, 0x04b0), (0x0409, 0x04b0), (0x0000, 0x04b0)];
        }
        let mut file_description = None;
        let mut product_name = None;
        for (lang, cp) in &translations {
            if file_description.is_none() {
                file_description = query_string(&buf, *lang, *cp, "FileDescription");
            }
            if product_name.is_none() {
                product_name = query_string(&buf, *lang, *cp, "ProductName");
            }
            if file_description.is_some() && product_name.is_some() {
                break;
            }
        }
        if file_description.is_none() && product_name.is_none() {
            Ok(None)
        } else {
            Ok(Some(RawMetadata {
                file_description,
                product_name,
            }))
        }
    }

    /// \VarFileInfo\Translation：(语言, 代码页) 的 u16 对数组。
    fn query_translations(buf: &[u8]) -> Option<Vec<(u16, u16)>> {
        let sub = wide("\\VarFileInfo\\Translation");
        let mut ptr: *mut c_void = core::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = unsafe {
            VerQueryValueW(
                buf.as_ptr() as *const c_void,
                PCWSTR(sub.as_ptr()),
                &mut ptr,
                &mut len,
            )
        };
        if !ok.as_bool() || ptr.is_null() || len < 4 {
            return None;
        }
        let words = len as usize / 2;
        let slice = unsafe { core::slice::from_raw_parts(ptr as *const u16, words) };
        Some(
            slice
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .collect(),
        )
    }

    /// \StringFileInfo\<lang><cp>\<key> 的字符串值（NUL 结尾的 UTF-16）。
    fn query_string(buf: &[u8], lang: u16, cp: u16, key: &str) -> Option<String> {
        let sub = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{key}"));
        let mut ptr: *mut c_void = core::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = unsafe {
            VerQueryValueW(
                buf.as_ptr() as *const c_void,
                PCWSTR(sub.as_ptr()),
                &mut ptr,
                &mut len,
            )
        };
        if !ok.as_bool() || ptr.is_null() {
            return None;
        }
        // len 是字符数（含结尾 NUL，各版本口径不一），同时以 NUL 为界
        // 并设上限，避免把脏数据整段读进来。
        let cap = (len as usize).min(512);
        let mut out = Vec::with_capacity(cap);
        for i in 0..cap {
            let ch = unsafe { *(ptr as *const u16).add(i) };
            if ch == 0 {
                break;
            }
            out.push(ch);
        }
        let s = String::from_utf16_lossy(&out);
        let s = s.trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn alias_table_hits_common_apps() {
        assert_eq!(alias_for("weixin.exe"), Some("微信"));
        assert_eq!(alias_for("Weixin.exe"), Some("微信"));
        assert_eq!(alias_for("WEIXIN.EXE"), Some("微信"));
        assert_eq!(alias_for("qq.exe"), Some("QQ"));
        assert_eq!(alias_for("verge-mihomo.exe"), Some("Clash Verge"));
        assert_eq!(alias_for("clash-verge.exe"), Some("Clash Verge"));
        assert_eq!(alias_for("qyclient.exe"), Some("爱奇艺"));
        assert_eq!(alias_for("cloudmusic.exe"), Some("网易云音乐"));
        assert_eq!(alias_for("chrome.exe"), Some("Chrome 浏览器"));
        assert_eq!(alias_for("msedge.exe"), Some("Edge 浏览器"));
    }

    #[test]
    fn alias_table_misses_unknown_apps() {
        assert_eq!(alias_for("some-random-tool.exe"), None);
        assert_eq!(alias_for("svchost.exe"), None);
        assert_eq!(alias_for(""), None);
    }

    #[test]
    fn usable_metadata_rejects_junk() {
        assert!(is_usable_metadata("Windows 服务宿主进程"));
        assert!(is_usable_metadata("Host Process for Windows Services"));
        assert!(!is_usable_metadata(""));
        assert!(!is_usable_metadata("   "));
        assert!(!is_usable_metadata("1.2.3.4"));
        assert!(!is_usable_metadata("v2.0"));
        assert!(!is_usable_metadata("16.0"));
        assert!(!is_usable_metadata("坏\u{fffd}名字"));
    }

    #[test]
    fn choose_prefers_file_description_then_product_name() {
        let meta = RawMetadata {
            file_description: Some("某应用".to_string()),
            product_name: Some("某产品".to_string()),
        };
        assert_eq!(choose_display_name("app.exe", Some(&meta)), "某应用");
        let meta = RawMetadata {
            file_description: None,
            product_name: Some("某产品".to_string()),
        };
        assert_eq!(choose_display_name("app.exe", Some(&meta)), "某产品");
    }

    #[test]
    fn choose_falls_back_when_metadata_unusable() {
        let meta = RawMetadata {
            file_description: Some("   ".to_string()),
            product_name: Some("1.0.2".to_string()),
        };
        assert_eq!(choose_display_name("app.exe", Some(&meta)), "app.exe");
        let meta = RawMetadata {
            file_description: Some("2.0".to_string()),
            product_name: Some("备用名".to_string()),
        };
        assert_eq!(choose_display_name("app.exe", Some(&meta)), "备用名");
        assert_eq!(choose_display_name("app.exe", None), "app.exe");
    }

    #[test]
    fn choose_alias_beats_metadata() {
        let meta = RawMetadata {
            file_description: Some("Tencent WeChat".to_string()),
            product_name: None,
        };
        assert_eq!(choose_display_name("weixin.exe", Some(&meta)), "微信");
    }

    #[test]
    fn label_combines_or_dedupes() {
        assert_eq!(format_label("微信", "weixin.exe"), "微信（weixin.exe）");
        assert_eq!(format_label("weixin.exe", "weixin.exe"), "weixin.exe");
        assert_eq!(format_label("Weixin.exe", "weixin.exe"), "Weixin.exe");
        assert_eq!(format_label("", "weixin.exe"), "weixin.exe");
    }

    /// 计数假来源：记录被调用次数，返回预置元数据。
    struct CountingProvider {
        calls: AtomicUsize,
        meta: Option<RawMetadata>,
    }

    impl CountingProvider {
        fn new(meta: Option<RawMetadata>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                meta,
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl MetadataProvider for CountingProvider {
        fn raw_metadata(&self, _exe_path: &str) -> Option<RawMetadata> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.meta.clone()
        }
    }

    #[test]
    fn resolver_caches_success_and_failure() {
        let meta = RawMetadata {
            file_description: Some("测试应用".to_string()),
            product_name: None,
        };
        let resolver = NameResolver::new(CountingProvider::new(Some(meta)));
        let path = "c:\\apps\\tool.exe";
        assert_eq!(resolver.display_name(path, "tool.exe"), "测试应用");
        assert_eq!(resolver.display_name(path, "tool.exe"), "测试应用");
        assert_eq!(
            resolver.label(path, "tool.exe"),
            "测试应用（tool.exe）"
        );
        assert_eq!(resolver.provider.calls(), 1, "同一路径只应解析一次");

        // 失败（None）同样缓存，不反复读。
        let resolver = NameResolver::new(CountingProvider::new(None));
        assert_eq!(resolver.display_name(path, "tool.exe"), "tool.exe");
        assert_eq!(resolver.display_name(path, "tool.exe"), "tool.exe");
        assert_eq!(resolver.provider.calls(), 1, "失败结果也应缓存");

        // 不同路径各自解析一次。
        assert_eq!(
            resolver.display_name("c:\\apps\\other.exe", "other.exe"),
            "other.exe"
        );
        assert_eq!(resolver.provider.calls(), 2);
    }

    #[test]
    fn resolver_synthetic_rows_skip_provider() {
        let resolver = NameResolver::new(CountingProvider::new(None));
        assert_eq!(resolver.display_name("system", "System"), "系统");
        assert_eq!(resolver.display_name("<unknown>", "Unknown"), "未归属流量");
        assert_eq!(resolver.label("system", "System"), "系统");
        assert_eq!(resolver.provider.calls(), 0, "合成行不应触发元数据读取");
    }
}
