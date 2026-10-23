# 敏感信息检查报告

首次检查：2026-10-23（覆盖 43 个提交）；本版更新：2026-10-23
发布准备轮次，覆盖范围推进到 v7 提交的全部提交，并按同一规则复扫。结论：在下列扫描规则与范围内**未发现
命中**（密钥、令牌、私人路径、私人邮箱等）；规则之外的情形不在
本检查覆盖内，本文件不作"绝对没有"的断言。

## 检查方法（可复核）

refs 范围：`git rev-list --all`（含上游 main、zh-dev 与标签
v0.1.0–v0.1.2）。逐提交对每棵树执行：

```
# 密钥类（路径限于源码与构建配置）
git grep -I -i -E "password|passwd|api[_-]?key|secret|token|BEGIN [A-Z ]*PRIVATE KEY|aws_access|ghp_[A-Za-z0-9]|sk-[A-Za-z0-9]{20}" \
  <提交> -- 'src/*' 'Cargo.toml' 'build.rs' '.cargo/*'
# 私人类（整棵树）
git grep -I -E "使用者姓名|QQ 号|/home/<账户名>/|C:\\Users\\|内部代理主机名|https?://[^/\s\"']+@" <提交>
# 文件名与作者清单
git log --all --pretty=format: --name-only | sort -u | grep -iE "\.env|\.pem|\.key|credential"
git log --all --format='%an <%ae>' | sort | uniq -c
find . -name "*.env" -o -name "*.pem" -o -name "*.key" -o -name "*credential*"
```

## 本轮结果（截至 v7 锚点 e0e2716，其时 50 个提交）

| 检查项 | 结果 | 说明 |
|---|---|---|
| 密码 / API 密钥 / 令牌 / 私钥头 | 未发现命中 | 唯一命中为 `src/backend/engine.rs` 的限速"令牌桶（token bucket）"术语，属假阳性 |
| 私钥 / .env / 凭据类文件 | 未发现 | 历史文件名清单与工作区均无此类文件 |
| 个人绝对路径 / 带凭据 URL | 未发现命中 | 源码中出现的均为 `%APPDATA%\Throttle` 等系统变量路径 |
| 邮箱（树内文件） | 仅 vendor 公开出处 | 唯一命中为 WinDivert 官方 README 中的作者地址（随官方分发物） |
| `.cargo/config.toml` | 安全 | 全文仅一行相对路径的 `WINDIVERT_PATH` 构建设置 |
| 日志文件 | 未发现 | 运行日志在使用者机器 `%APPDATA%\Throttle\throttle.log` 生成，不入库 |
| exe 字符串抽查 | 见 BUILD.md | 检出构建机账户目录的 registry 路径残留（构建环境信息），无凭据模式命中；正式构建建议按 BUILD.md 用 `--remap-path-prefix` 剥离 |

## 提交者信息（2026-10-23 已处理）

- 全部提交的作者现为三方：上游 Francesco Gruosso（36 个，公开
  noreply 地址与 `hello@franwbu.com`，均为上游公开仓库自身的提交
  元数据）、上游贡献者 MCOfficer（5 个，公开地址）、本项目发布者
  **cruelsummera1-collab
  `337259847+cruelsummera1-collab@users.noreply.github.com`**（13 个，
  即全部改造与发布准备提交；该 noreply 邮箱关联发布者的 GitHub
  账号）。
- 历史记录：这 13 个提交原先的作者是开发环境预置的本地占位身份
  `Muse <muse@localhost>`（非真实邮箱）。发布者于 2026-10-23
  决定改用本人 GitHub 身份，已按 `main..zh-dev` 范围重写这 13 个
  提交的作者与提交者；上游 41 个提交与全部标签未动，重写前后每
  个提交的文件树逐一比对一致（内容零变化），仓库本地配置的提交
  身份也已同步改为发布者身份。

## 其他说明

- `Cargo.toml` 原 `repository` 字段指向上游仓库；发布准备中
  一度移除，2026-10-23 发布仓库确定后已改为发布者自己的仓库
  地址（`github.com/cruelsummera1-collab/throttle-zh`），程序
  "关于"中的仓库链接随之指向新仓库；上游出处另由 README 与
  LICENSE 中的标注保留。
- 本文件覆盖的历史在 2026-10-23 作者重写后已按"检查方法"对
  全量提交复扫（结果随重写交付记录在案）；正式推送前请再复扫
  一次并更新本节的提交数。
