# 构建说明（Windows 10/11 x64）

## 源码与基线
- 基于 https://github.com/FrancescoCoding/Throttle 的 `v0.1.2` 标签
  （commit 5ddccc7），在本仓库 `zh-dev` 分支上改造。
- 随交付的源码目录保留了 `.git`，可用 `git log` / `git diff v0.1.2` 查看
  全部改动，`git checkout v0.1.2` 可回到原版。
- 依赖版本以仓库内 **Cargo.lock** 锁定为准，构建时加 `--locked`
  可保证解析结果与本文档记录一致。
- WinDivert 不走 crates.io 下载：`.cargo/config.toml` 将
  `WINDIVERT_PATH` 指向仓库内 `vendor/WinDivert-2.2.2-A/x64`
  （相对路径），windivert-sys 据此对 WinDivert.dll 的导入库做
  **动态链接**（运行时加载 DLL），不静态链接、不重新编译驱动。

## 工具链（2026-10-23 本轮实测值）

| 工具 | 版本 |
|---|---|
| rustc / cargo | 1.99.1（rustup stable，rustc commit 58cf8f9c3） |
| 构建目标 | `x86_64-pc-windows-gnu` |
| x86_64-w64-mingw32-gcc | 13.2.0（Ubuntu 包 gcc-mingw-w64-x86-64 13.2.0-6ubuntu1+26.1） |
| windres / objdump（binutils） | 2.41.90.20240122 |
| mingw-w64 运行时/头文件 | 11.0.1 |
| 构建机系统 | Linux x86_64（Ubuntu 24.04），2 核 / 7.7GB |

## 方式一：Windows 本机原生构建（推荐，产物最完整）
1. 安装 Rust：https://rustup.rs ，选 stable + 默认 MSVC 工具链。
2. 安装 Visual Studio 2022 Build Tools，勾选"使用 C++ 的桌面开发"。
3. 在仓库根目录执行：
   ```
   cargo build --release --locked
   ```
4. 产物：`target\release\throttle.exe`。
   本机构建时 build.rs 会自动嵌入 requireAdministrator 清单和程序
   图标；VERSIONINFO 产品信息（见下）在原生与交叉构建下都会嵌入。
5. 把驱动文件复制到 exe 同目录（缺一不可）：
   ```
   copy vendor\WinDivert-2.2.2-A\x64\WinDivert.dll target\release\
   copy vendor\WinDivert-2.2.2-A\x64\WinDivert64.sys target\release\
   ```
6. 以管理员身份运行 `throttle.exe`。

## 方式二：Linux 交叉构建（本项目发布包采用）
环境：Ubuntu 24.04 + 上表工具链。

```
rustup target add x86_64-pc-windows-gnu
sudo apt-get install -y mingw-w64 gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64
cargo build --release --locked --target x86_64-pc-windows-gnu
cp vendor/WinDivert-2.2.2-A/x64/WinDivert.dll vendor/WinDivert-2.2.2-A/x64/WinDivert64.sys \
   target/x86_64-pc-windows-gnu/release/
```

注意：交叉构建时 build.rs 的**清单（requireAdministrator）与图标**
嵌入步骤按宿主系统判断会被跳过，因此交叉构建的 exe **不带管理员
清单**——在 Windows 上首次运行请手动右键"以管理员身份运行"。
**VERSIONINFO 产品信息不受此限**：build.rs 按*目标*系统判断，
交叉构建同样嵌入（产品名"流量监控"、说明、版本 0.1.2、版权行），
可用 `objdump -h` 查看 .rsrc 段、解析 RT_VERSION 资源核对。
功能代码与本机构建完全一致。如需带清单的正式包，请用方式一构建。

## exe 产品元数据与签名状态
- build.rs（zh-dev 提交 cc17927）用 winresource 为 Windows 目标
  写入 VERSIONINFO：ProductName「流量监控」、FileDescription
  「流量监控（Throttle 中文增强版）」、FileVersion/ProductVersion
  0.1.2、OriginalFilename throttle.exe、LegalCopyright（上游版权 +
  修改部分见 LICENSE 的简式表述）。交叉构建的新 exe 已实际解析
  RT_VERSION 验证写入成功、中文无乱码（见测试报告.md）。
- **签名状态：未签名。** 本项目没有 Authenticode 代码签名证书，
  未做任何签名（也未用自签名证书冒充）。正式签名需要：发布者
  自备代码签名证书（OV/EV），在 Windows 上用 signtool 对 exe
  （及安装包）签名并加盖时间戳；驱动文件 WinDivert64.sys 为
  WinDivert 官方已签名文件，保持原样、切勿重签或改动。

## 测试
- 纯逻辑单元测试（告警状态机、历史记录模块）可在任意平台运行：
  两个模块仅依赖 std，可独立编译测试，例如：
  ```
  rustc --edition 2024 --test src/alert.rs -o /tmp/alert_test && /tmp/alert_test
  rustc --edition 2024 --test src/history.rs -o /tmp/history_test && /tmp/history_test
  ```
- 全量类型检查（含测试代码编译）：
  ```
  cargo check --target x86_64-pc-windows-gnu
  cargo check --tests --target x86_64-pc-windows-gnu
  ```

## 打包步骤（发布包）
1. 完成方式二构建，取 `target/x86_64-pc-windows-gnu/release/throttle.exe`。
2. 按下表组装暂存目录（`release/` 子目录为根），其中
   `Throttle-LICENSE.txt` 使用仓库 LICENSE 的**更正版**文本
   （WinDivert 许可措辞已按官方文本修正），字体许可证全文取自
   epaint_default_fonts 0.36.2 crate 随附文件。
3. 将 `release/` 打成 zip，并记录 zip 与 exe 的 SHA-256。

### 产物清单（v7 包，2026-10-23 打包记录，哈希经发布前复查核对）

| 文件 | 大小（字节） | 用途 |
|---|---|---|
| throttle.exe | 6,731,776 | 主程序（PE32+ x86-64 GUI，含 VERSIONINFO，未签名）；SHA-256 `2bd5a8fbcfed159134c3d605a6df02251f6232d5f5cfd8186480918b0c5242d1` |
| WinDivert.dll | 47,616 | WinDivert 2.2.2 用户态库（运行必需，官方原文件） |
| WinDivert64.sys | 94,144 | WinDivert 2.2.2 x64 驱动（运行必需，官方原文件） |
| WinDivert-LICENSE.txt | 61,349 | WinDivert 许可证全文（LGPL v3 / GPL v2） |
| Throttle-LICENSE.txt | 2,683 | 本体 MIT 许可 + WinDivert 分发说明（更正版）+ 修改部分版权 |
| licenses/Hack-LICENSE.txt 等 4 份 | 1,069–4,673 | 内嵌字体许可证全文（见 THIRD_PARTY_LICENSES.md 第 4 节） |

发布包 `throttle-zh-release-v7.zip` 共 3,401,049 字节，SHA-256
`ad2bb1464d5e06e858c657a351e658ba628cc3baf402475de99b2ffd35e08658`。
说明：上表数值引自 v7 打包记录并在发布前复查中与实物哈希核对
一致；**本轮发布准备未重新执行构建**，上述构建步骤对当前源码的
可复现性标注为尚未复验；exe 由哪个提交构建无法仅凭哈希确认，
正式发布时请保留当次构建日志与产物哈希（见文末建议）。

## 二进制内路径说明（strings 扫描分类，2026-10-23 实测）

对发布 exe 做 strings 扫描，按来源分类如下（新旧两版 exe 结论一致）：

- **(a) cargo registry 依赖源码路径（约 1,307 处）**：形如
  `<构建机账户目录>/.cargo/registry/src/index.crates.io-<哈希>/<crate>/src/…`，
  来自各依赖 crate 在 panic/断言位置信息中记录的源码路径，属通用
  构建痕迹。
- **(b) 构建机工作区路径（1 处）**：构建产物中间文件路径
  （`…/throttle-dev/target/…/out/wgl_extra_bindings.rs`）。
  (a)(b) 中的 `<构建机账户目录>` 是**构建机（云主机）的账户目录**，仅
  暴露构建环境账户名，不含软件使用者的任何个人信息、真实姓名、
  邮箱或凭据。
- **(c) 个人敏感信息：未发现。** `C:\Users\` 路径 0 处、带凭据的
  URL（`http://user:pass@…`）0 处、API 密钥/令牌/私钥头模式 0 处。

**正式发布构建建议**：如介意 (a)(b) 的构建机路径，可在构建时用
`--remap-path-prefix` 剥离，例如
`RUSTFLAGS="--remap-path-prefix=<构建机账户目录>=." cargo build …`，
或在路径中性的容器/CI 环境中构建。已交付的各版发布包为保持可比性未启用 remap，此建议留待正式发布时执行。
