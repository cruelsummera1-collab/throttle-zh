# 第三方许可证说明

本文件记录本项目（Throttle 中文增强版）所含第三方成果的许可证情况。
第 3 节与附表已于 2026-10-23 按第 7 个内部迭代（v7，提交 06193e7）的
`Cargo.lock` 与 Windows 目标依赖解析重新导出；其余各节的实读结论
沿用并注明出处。未实读确认的内容列入文末"待确认事项"，不作推测性
结论。本文件不构成法律意见。

## 1. 上游项目 Throttle

- 许可证：**MIT License**（实读 `LICENSE` 文件全文确认）。
- 版权声明原文：`Copyright (c) 2026 Francesco Gruosso`
- 仓库根目录的 `LICENSE` 以该文件为底本：**MIT 许可证正文一字未
  改**；其后 bundling 说明段中关于 WinDivert 许可的一句存在事实
  错误，已按 WinDivert 官方文本更正（更正内容与依据见第 2 节，该段
  已在文件内以 [Wording note] 标明）；最后在分隔线之后追加了本项目
  修改部分的版权行（署名暂为占位符「本项目维护者」，**发布前必须
  替换为发布者的真实署名**，见待确认事项 3）。
- 仓库内未发现其他许可证文件（无 COPYING、NOTICE）；源码文件头为
  模块文档注释，不含独立版权行。另有一处 UI 版权展示：`src/gui.rs`
  的"关于"窗口中显示 `Copyright © 2026 Francesco Gruosso`，改造中
  已保留。

## 2. WinDivert（驱动与 DLL）

- 版本：**2.2.2**（实读 `vendor/WinDivert-2.2.2-A/VERSION`）。
- 许可证原文（实读 `vendor/WinDivert-2.2.2-A/LICENSE` 第 1–3 行）：
  "WinDivert is dual-licensed under your choice of the GNU Lesser General
  Public License (LGPL) Version 3 or the GNU General Public License (GPL)
  Version 2." —— 即**双许可是 LGPL v3 与 GPL v2 两项开源许可之间的
  任选其一，不含任何专有/商业许可选项**。`vendor/WinDivert-2.2.2-A/README`
  第 3 节 License 亦为同一表述。本项目按 LGPL v3 一侧分发。
- **措辞更正记录**：上游 v0.1.2 仓库的 `LICENSE` 第 27 行 bundling
  说明原文为 "the GNU Lesser General Public License v3 (with a dual
  GPL/proprietary option)"，上游 `README.md` 亦有 "(with a GPL/proprietary
  dual option)" 的同类表述——两处均把 "dual" 误写成含 proprietary
  选项，与 WinDivert 官方文本不符。**本仓库根目录的 `LICENSE` 与
  `README.md` 已于 2026-10-23 发布准备轮次同步更正**（与发布包内
  `Throttle-LICENSE.txt` 一致）；上游仓库原文未动，此处仅记录差异。
- 链接方式（实测证据）：`windivert-sys 0.10.0` 的构建脚本在走
  `WINDIVERT_PATH` 路线时输出 `cargo:rustc-link-lib=dylib=WinDivert`
  （构建产物目录中该 crate 的 build output 原文），即通过导入库
  **动态链接** WinDivert.dll、运行时加载；其 `static` / `vendored`
  特性未启用（本项目 Cargo.toml 未声明这些特性，`.cargo/config.toml`
  仅设置 WINDIVERT_PATH 指向 vendor 的 x64 目录）。
- 分发方式与条件吻合情况：
  - `WinDivert.dll`、`WinDivert64.sys` 以**独立文件**随发布包分发，
    未静态链接进 throttle.exe；
  - 发布包内附有 WinDivert 许可证全文（`WinDivert-LICENSE.txt`；
    仓库内同一全文在 `licenses/WinDivert-LICENSE.txt`）；
  - 文件保持原样、可替换（用户可自行换成官方同版本文件）。
- 仓库 `vendor/WinDivert-2.2.2-A/` 还包含官方的头文件、导入库（.lib）、
  x86 版本与文档，均为 WinDivert 官方发布物的一部分，同属上述许可。

## 3. Rust 依赖（crates）

导出方法与统计口径（可复核）：

```
cargo metadata --format-version 1 --locked \
  --filter-platform x86_64-pc-windows-gnu
```

- **口径**：上式输出的 package 实例数，含本项目本体 `throttle` 1 个；
  按各 crate 在 crates.io 登记的 `license` 字段（SPDX 表达式）统计。
- **v7 实测（2026-10-23，锁文件为提交 06193e7 的 Cargo.lock）**：
  Windows x64 目标解析共 **217 个 package（其中第三方 216 个）**；
  不过滤平台的全量解析为 476 个 package（410 个不重名）。旧版文档
  中的 227 / 408 是 2026-10-23 早前一版锁文件的同口径数字，已被
  本节取代，引用时请勿混用。
- 第三方 216 个的许可证分布：

| 许可证表达式 | 数量 |
|---|---|
| MIT OR Apache-2.0 | 116 |
| MIT | 26 |
| Unicode-3.0 | 18 |
| Apache-2.0 OR MIT | 17 |
| Apache-2.0 | 8 |
| MIT OR Apache-2.0 OR Zlib | 5 |
| MIT/Apache-2.0 | 4 |
| Zlib OR Apache-2.0 OR MIT | 2 |
| Unlicense OR MIT | 2 |
| BSL-1.0 | 2 |
| Zlib | 2 |
| MIT OR Zlib OR Apache-2.0 | 2 |
| BSD-3-Clause OR Apache-2.0 | 2 |
| LGPL-3.0-or-later | 2 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| Apache-2.0 AND MIT | 1 |
| (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | 1 |
| ISC | 1 |
| Apache-2.0 OR GPL-2.0-only | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |

以下为**非 MIT/Apache-2.0 主流组合**的少数派，单独列出（均为多许可
择一或宽松许可；是否与本项目的分发方式冲突属法律判断，本文不下
结论，相关项见待确认事项）：

- **windivert 0.6.0、windivert-sys 0.10.0 — LGPL-3.0-or-later**：
  WinDivert 的 Rust 绑定层：绑定 crate 的 **Rust 代码**编译进 exe，
  但 WinDivert 本体（C 库）经导入库动态链接、运行时加载 DLL（证据
  见第 2 节）。合规口径见待确认事项 1。
- **epaint_default_fonts 0.36.2 — (MIT OR Apache-2.0) AND OFL-1.1 AND
  Ubuntu-font-1.0**：egui 框架内嵌的默认字体数据，随二进制分发，
  逐字体核实结果见第 4 节。
- **tray-icon 0.21.3 — MIT OR Apache-2.0；muda 0.17.2 — Apache-2.0
  OR MIT**：v6 起新增的托盘依赖（muda 为 tray-icon 的传递依赖），
  二者均在上述 Windows 解析内、且 exe 字符串中可检出其编译路径；
  其许可全文随各自 crate 在 crates.io 分发，本仓库未单独收入
  （见待确认事项 5）。
- **self_cell 1.3.0 — Apache-2.0 OR GPL-2.0-only**：双许可择一。
- **Unicode-3.0**（含 icu_*、zerovec、zerotrie、tinystr、
  unicode-ident 等，合计 19 个表达式含 Unicode-3.0）：Unicode
  许可证 v3，要求保留声明。
- **BSL-1.0**：clipboard-win 5.4.1、error-code 3.4.0（Boost 许可）。
- **BSD-2-Clause / BSD-3-Clause 组合**：adler2、moxcms、pxfm、
  zerocopy 等（均为 "OR" 组合中的一侧）。
- **Zlib**：foldhash 0.2.0、zlib-rs 0.6.8 等。**ISC**：libloading
  0.8.9。**0BSD / Unlicense**：见附表全量清单。
- 上一版附表中列有的 **wgpu 家族（wgpu / wgpu-core / wgpu-hal /
  naga 等）已随 v7 渲染器切换（wgpu → glow）从锁文件移除**，附表
  不再包含；glow 0.17.0（MIT OR Apache-2.0 OR Zlib）在附表内。

完整 216 个第三方 crate 的名称/版本/许可证见文末附表。

## 4. 内嵌字体（epaint_default_fonts 0.36.2，随 exe 分发）

核实方法：实读该 crate 的 `src/lib.rs`（四个 `include_bytes!` 常量），
并对交付 exe 做 strings 扫描比对字体 name 表记录；许可证文本取自该
crate `fonts/` 目录随附文件，已收入本目录 `licenses/` 子目录。

| 字体 | 文件 | 许可证 | 核实证据 |
|---|---|---|---|
| Hack Regular 3.003 | Hack-Regular.ttf | MIT（© 2018 Source Foundry Authors）；其 Bitstream Vera 派生部分适用 Bitstream Vera License（© 2003 Bitstream, Inc.，保留字名 "Bitstream"/"Vera"）；DejaVu 部分为公有领域贡献 | crate 内 `Hack-Regular.txt` 全文；exe 内 name 表含 "Copyright (c) 2018 Source Foundry Authors / Copyright (c) 2003 by Bitstream, Inc." 及上述许可说明串 |
| Noto Emoji Regular 1.05 | NotoEmoji-Regular.ttf | SIL Open Font License 1.1（© 2013 Google Inc.） | crate 内 `OFL.txt`；exe 内 name 表含 "This Font Software is licensed under the SIL Open Font License, Version 1.1" |
| Ubuntu Light 0.83 | Ubuntu-Light.ttf | Ubuntu Font Licence 1.0（© 2011 Canonical Ltd.） | crate 内 `UFL.txt`；exe 内 name 表含 "Copyright 2011 Canonical Ltd. Licensed under the Ubuntu Font Licence 1.0" |
| emoji-icon-font | emoji-icon-font.ttf | MIT（© 2014 John Slegers） | crate 内 `emoji-icon-font-mit-license.txt`；exe strings 命中 "emoji-icon" 标识；该字体 name 表未检出版权串，以 crate 随附许可文件为据 |

说明：exe 中**未**检出 "Bitstream Vera"、"SIL Open Font License" 等
*许可全文文本*字符串（命中均为字体 name 表的版权/许可摘要字段），
即许可全文并未被编译进二进制——这正是发布包需以独立文件附上全文的
原因，现已备齐于 `licenses/`，并收入新发布包暂存（见 package/）。

## 5. 界面中文字体（不随程序分发）

程序运行时从系统目录加载微软雅黑（`C:\Windows\Fonts\msyh.ttc`，
失败时回退宋体/黑体），**不打包、不分发任何字体文件**，因此不存在
字体再分发的许可问题。egui 框架自带的内嵌字体数据见第 4 节。

## 待确认事项

1. **LGPL Rust 绑定（windivert / windivert-sys）的合规方式**：
   两绑定 crate（LGPL-3.0-or-later）的 Rust 代码编译进 exe；WinDivert
   本体经导入库动态链接（第 2 节有构建证据）。LGPL 对"库代码进入
   二进制"的通行要求是让用户能够替换该库并重新链接；本项目以
   **完整源码开源**方式发布（用户可自行替换 crate 版本并重新
   构建），是否足以满足 LGPL 的要求属法律解释问题，本文件不下
   结论。发布前建议：在 README/发布说明中明确写出 windivert 绑定
   的版本与替换构建方法，或就此咨询专业人士后再正式发布。
2. ~~**egui 内嵌字体许可证文本缺失**~~ —— **已落实**：四个内嵌
   字体逐一核实（第 4 节），其许可证全文已收入仓库 `licenses/`
   并放入发布包，旧发布包未附文本的问题在新包中不再存在。
3. **修改部分署名占位符**：`LICENSE` 中「本项目维护者」需在发布
   前替换为发布者的真实署名（GitHub 用户名或姓名）。**本项未解决
   前不建议正式发布。**
4. **上游 LICENSE 年份**：上游版权年份原文为 2026（`Copyright (c)
   2026 Francesco Gruosso`），系上游文件原文如此，本项目原样保留、
   未作改动；如与 GitHub 上游现状不一致，以上游仓库为准，发布前
   可再核对一次。
5. **逐项完整性的边界**：附表基于 crates.io 登记的 `license` 元
   数据字段自动导出，未逐一实读全部 216 个 crate 的随附许可文件；
   除字体（第 4 节）与 WinDivert（第 2 节）外，各 crate 的许可证
   全文未收入本仓库（随各自 crate 在 crates.io 分发）。如发布者
   要求"全文齐备"，需另行逐项收集，本文件不声称已达到该标准。

## 附表：Windows 目标全部第三方依赖（216 个，v7 / 06193e7）

| crate | 版本 | 许可证 |
|---|---|---|
| accesskit | 0.24.1 | MIT OR Apache-2.0 |
| accesskit_consumer | 0.35.0 | MIT OR Apache-2.0 |
| accesskit_windows | 0.32.1 | MIT OR Apache-2.0 |
| accesskit_winit | 0.32.2 | Apache-2.0 |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| anyhow | 1.0.103 | MIT OR Apache-2.0 |
| arboard | 3.6.1 | MIT OR Apache-2.0 |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| bytemuck_derive | 1.12.1 | Zlib OR Apache-2.0 OR MIT |
| byteorder-lite | 0.1.0 | Unlicense OR MIT |
| cc | 1.4.6 | MIT OR Apache-2.0 |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 |
| cfg_aliases | 0.2.2 | MIT |
| clipboard-win | 5.4.1 | BSL-1.0 |
| color | 0.3.3 | Apache-2.0 OR MIT |
| crc32fast | 1.5.2 | MIT OR Apache-2.0 |
| crossbeam-channel | 0.5.16 | MIT OR Apache-2.0 |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 |
| cursor-icon | 1.2.0 | MIT OR Apache-2.0 OR Zlib |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 |
| document-features | 0.2.12 | MIT OR Apache-2.0 |
| dpi | 0.1.2 | Apache-2.0 AND MIT |
| ecolor | 0.36.2 | MIT OR Apache-2.0 |
| eframe | 0.36.2 | MIT OR Apache-2.0 |
| egui | 0.36.2 | MIT OR Apache-2.0 |
| egui-winit | 0.36.2 | MIT OR Apache-2.0 |
| egui_extras | 0.36.2 | MIT OR Apache-2.0 |
| egui_glow | 0.36.2 | MIT OR Apache-2.0 |
| egui_plot | 0.37.0 | MIT OR Apache-2.0 |
| either | 1.18.0 | MIT OR Apache-2.0 |
| emath | 0.36.2 | MIT OR Apache-2.0 |
| embed-manifest | 1.5.0 | MIT |
| enum-map | 2.7.3 | MIT OR Apache-2.0 |
| enum-map-derive | 0.17.0 | MIT OR Apache-2.0 |
| epaint | 0.36.2 | MIT OR Apache-2.0 |
| epaint_default_fonts | 0.36.2 | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| error-code | 3.4.0 | BSL-1.0 |
| etherparse | 0.13.0 | MIT OR Apache-2.0 |
| euclid | 0.22.14 | MIT OR Apache-2.0 |
| fax | 0.2.7 | MIT |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| fearless_simd | 0.4.1 | Apache-2.0 OR MIT |
| find-msvc-tools | 0.1.12 | MIT OR Apache-2.0 |
| flate2 | 1.1.10 | MIT OR Apache-2.0 |
| foldhash | 0.2.0 | Zlib |
| font-types | 0.12.5 | MIT OR Apache-2.0 |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| getrandom | 0.3.4 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| gl_generator | 0.14.0 | Apache-2.0 |
| glifo | 0.2.0 | Apache-2.0 OR MIT |
| glow | 0.17.0 | MIT OR Apache-2.0 OR Zlib |
| glutin | 0.32.3 | Apache-2.0 |
| glutin-winit | 0.5.0 | MIT |
| glutin_egl_sys | 0.7.1 | Apache-2.0 |
| glutin_wgl_sys | 0.6.1 | Apache-2.0 |
| guillotiere | 0.7.0 | MIT/Apache-2.0 |
| half | 2.7.1 | MIT OR Apache-2.0 |
| harfrust | 0.12.0 | MIT |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| icu_collections | 2.3.0 | Unicode-3.0 |
| icu_locale_core | 2.3.0 | Unicode-3.0 |
| icu_normalizer | 2.3.0 | Unicode-3.0 |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 |
| icu_properties | 2.3.0 | Unicode-3.0 |
| icu_properties_data | 2.3.0 | Unicode-3.0 |
| icu_provider | 2.3.1 | Unicode-3.0 |
| idna | 1.1.0 | MIT OR Apache-2.0 |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT |
| image | 0.25.10 | MIT OR Apache-2.0 |
| indexmap | 2.14.2 | Apache-2.0 OR MIT |
| itertools | 0.15.0 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| jobserver | 0.1.35 | MIT OR Apache-2.0 |
| keyboard-types | 0.7.0 | MIT OR Apache-2.0 |
| khronos_api | 3.1.0 | Apache-2.0 |
| kurbo | 0.13.1 | Apache-2.0 OR MIT |
| lazy_static | 1.5.0 | MIT OR Apache-2.0 |
| libloading | 0.8.9 | ISC |
| linebender_resource_handle | 0.1.1 | Apache-2.0 OR MIT |
| litemap | 0.8.3 | Unicode-3.0 |
| litrs | 1.0.0 | MIT OR Apache-2.0 |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| log | 0.4.34 | MIT OR Apache-2.0 |
| memchr | 2.8.3 | Unlicense OR MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| mime_guess2 | 2.3.1 | MIT |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| muda | 0.17.2 | Apache-2.0 OR MIT |
| nohash-hasher | 0.2.0 | Apache-2.0 OR MIT |
| nu-ansi-term | 0.50.3 | MIT |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 |
| peniko | 0.6.1 | Apache-2.0 OR MIT |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| phf | 0.11.3 | MIT |
| phf_generator | 0.11.3 | MIT |
| phf_macros | 0.11.3 | MIT |
| phf_shared | 0.11.3 | MIT |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| png | 0.18.1 | MIT OR Apache-2.0 |
| polycool | 0.4.0 | MIT OR Apache-2.0 |
| potential_utf | 0.1.6 | Unicode-3.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| profiling | 1.0.18 | MIT OR Apache-2.0 |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 |
| quick-error | 2.0.1 | MIT/Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| rand | 0.8.8 | MIT OR Apache-2.0 |
| rand_core | 0.6.4 | MIT OR Apache-2.0 |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib |
| read-fonts | 0.41.0 | MIT OR Apache-2.0 |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| self_cell | 1.3.0 | Apache-2.0 OR GPL-2.0-only |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| serde_spanned | 1.1.1 | MIT OR Apache-2.0 |
| sharded-slab | 0.1.7 | MIT |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| simd-adler32 | 0.3.10 | MIT |
| siphasher | 1.0.3 | MIT/Apache-2.0 |
| skrifa | 0.44.0 | MIT OR Apache-2.0 |
| smallvec | 1.16.1 | MIT OR Apache-2.0 |
| smol_str | 0.2.2 | MIT OR Apache-2.0 |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| syn | 3.0.6 | MIT OR Apache-2.0 |
| synstructure | 0.14.0 | MIT |
| thiserror | 1.0.69 | MIT OR Apache-2.0 |
| thiserror | 2.0.20 | MIT OR Apache-2.0 |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.20 | MIT OR Apache-2.0 |
| thread_local | 1.1.10 | MIT OR Apache-2.0 |
| tiff | 0.11.3 | MIT |
| tinystr | 0.8.4 | Unicode-3.0 |
| toml | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_datetime | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_parser | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_writer | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |
| tracing | 0.1.44 | MIT |
| tracing-attributes | 0.1.31 | MIT |
| tracing-core | 0.1.36 | MIT |
| tracing-log | 0.2.0 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| tray-icon | 0.21.3 | MIT OR Apache-2.0 |
| unicase | 2.9.0 | MIT OR Apache-2.0 |
| unicode-general-category | 1.1.0 | Apache-2.0 |
| unicode-ident | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 |
| url | 2.5.8 | MIT OR Apache-2.0 |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT |
| uuid | 1.26.1 | Apache-2.0 OR MIT |
| vello_common | 0.1.0 | Apache-2.0 OR MIT |
| vello_cpu | 0.1.0 | Apache-2.0 OR MIT |
| version_check | 0.9.5 | MIT/Apache-2.0 |
| web-time | 1.1.0 | MIT OR Apache-2.0 |
| webbrowser | 1.2.4 | MIT OR Apache-2.0 |
| weezl | 0.1.12 | MIT OR Apache-2.0 |
| windivert | 0.6.0 | LGPL-3.0-or-later |
| windivert-sys | 0.10.0 | LGPL-3.0-or-later |
| windows | 0.39.0 | MIT OR Apache-2.0 |
| windows | 0.48.0 | MIT OR Apache-2.0 |
| windows | 0.62.2 | MIT OR Apache-2.0 |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-future | 0.3.2 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 |
| windows-sys | 0.60.2 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| windows-targets | 0.48.5 | MIT OR Apache-2.0 |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 |
| windows-targets | 0.53.5 | MIT OR Apache-2.0 |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.39.0 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.48.5 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.53.1 | MIT OR Apache-2.0 |
| winit | 0.30.13 | Apache-2.0 |
| winnow | 1.0.4 | MIT |
| winreg | 0.56.0 | MIT |
| winresource | 0.1.31 | MIT |
| winrt-toast | 0.1.1 | MIT |
| writeable | 0.6.4 | Unicode-3.0 |
| xml-rs | 0.8.29 | MIT |
| yoke | 0.8.3 | Unicode-3.0 |
| yoke-derive | 0.8.3 | Unicode-3.0 |
| zerocopy | 0.8.57 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.57 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerofrom | 0.1.8 | Unicode-3.0 |
| zerofrom-derive | 0.1.8 | Unicode-3.0 |
| zerotrie | 0.2.5 | Unicode-3.0 |
| zerovec | 0.11.8 | Unicode-3.0 |
| zerovec-derive | 0.11.6 | Unicode-3.0 |
| zlib-rs | 0.6.8 | Zlib |
| zmij | 1.0.23 | MIT |
| zune-core | 0.5.3 | MIT OR Apache-2.0 OR Zlib |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib |
