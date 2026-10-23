# 敏感信息检查报告

首次检查：2026-10-23（覆盖 43 个提交）；本版更新：2026-10-23
发布准备轮次，覆盖范围推进到提交 `06193e7`（v7）的全部 **50 个
提交**，并按同一规则复扫。结论：在下列扫描规则与范围内**未发现
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

## 本轮结果（截至 06193e7，50 个提交）

| 检查项 | 结果 | 说明 |
|---|---|---|
| 密码 / API 密钥 / 令牌 / 私钥头 | 未发现命中 | 唯一命中为 `src/backend/engine.rs` 的限速"令牌桶（token bucket）"术语，属假阳性 |
| 私钥 / .env / 凭据类文件 | 未发现 | 历史文件名清单与工作区均无此类文件 |
| 个人绝对路径 / 带凭据 URL | 未发现命中 | 源码中出现的均为 `%APPDATA%\Throttle` 等系统变量路径 |
| 邮箱（树内文件） | 仅 vendor 公开出处 | 唯一命中为 WinDivert 官方 README 中的作者地址（随官方分发物） |
| `.cargo/config.toml` | 安全 | 全文仅一行相对路径的 `WINDIVERT_PATH` 构建设置 |
| 日志文件 | 未发现 | 运行日志在使用者机器 `%APPDATA%\Throttle\throttle.log` 生成，不入库 |
| exe 字符串抽查 | 见 BUILD.md | 检出构建机账户目录的 registry 路径残留（构建环境信息），无凭据模式命中；正式构建建议按 BUILD.md 用 `--remap-path-prefix` 剥离 |

## 提交者信息（发布者须知，非泄密判定）

- 全部 50 个提交的作者只有三方：上游 Francesco Gruosso（36 个，
  GitHub noreply 地址与 `hello@franwbu.com`，均为上游公开仓库自身
  的提交元数据）、上游贡献者 MCOfficer（5 个，公开地址）、本项目
  的 **`Muse <muse@localhost>`（9 个，即全部改造提交）**。
- `Muse <muse@localhost>` 是开发环境（云端虚拟机）预置的本地
  git 身份：`localhost` 不是可投递的真实邮箱，本次扫描未发现它
  与任何真实姓名或联系方式的关联记录。推送到 GitHub 后，它会以
  纯文本「Muse」显示在改造提交的作者位置，**不会**关联到任何
  GitHub 账号。
- 是否处理、如何处理由发布者决定，本准备工作**未擅自改写
  历史**。可选方式：①保持原样（公开承认 AI 协助开发，哈希
  不变）；②在**首次推送前**用 `git filter-repo` 只把这 9 个（及
  后续发布准备）提交的作者/提交者替换为发布者的真实姓名与邮箱
  （上游提交不动；所有被改提交的哈希会变化，未发布时无外部
  影响）；③历史保留、仅今后提交改用发布者身份。

## 其他说明

- `Cargo.toml` 原 `repository` 字段指向上游仓库，本轮发布准备
  已**移除**该字段（避免本仓库元数据指向他仓）；上游出处改由
  README 与 LICENSE 中的标注保留，不影响溯源。
- 本文件覆盖的是截至 06193e7 的历史与本轮工作区。其后的发布
  准备提交（文档与许可材料）已在提交前逐文件按上表规则检查；
  正式推送前请按"检查方法"一节对全量历史再复扫一次并更新本节
  的提交数。
