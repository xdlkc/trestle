# Trestle（栈架）

[English](./README.md) | 简体中文

> 工匠支起案板、摆放图纸与校核量具的栈架：承托任务脉络、交接简报与验证回执，而不侵入外部业务源码仓库。

**Trestle（栈架）** 是一个本地优先、与具体业务领域无关的个人任务、交接与交付核验工作台（Rust 实现，`edition = "2024"`，命令行入口 `wb`）。

---

## 设计理念与系统边界

- **单一静态二进制**：编译产物为 `wb`（`target/release/wb` 或 `target/debug/wb`），运行时零 Python / Node.js 依赖（`Git` 为可选工具，未安装时仍可正常进行任务管理与本地看板浏览）。
- **工作台与源码严格分离**：工作台目录仅保存任务登记、时间顺序进展、交付声明、验证回执与方案/分析文档，绝不复制外部业务仓库源码（`repos_root`）。
- **计划声明与事实观察分离**：
  - `tasks/<task-id>/delivery.yaml` 保存显式声明的交付目标（`repo`、`worktree`、`source_branch`、`target_branch`、`compare_base`、`allowed_paths`、`depends_on`）与待完成动作，受乐观修订号 `revision` 保护并自动归档不可变快照至 `tasks/<task-id>/delivery-history/`。
  - `.wb-runtime/observations/<task-id>.json` 保存 `wb sync` 只读采集的外部 Git 事实（实际分支、完整 `HEAD` 提交、未保存修改数、文件 SHA-256、分支偏移与越界路径），绝不覆盖 `delivery.yaml` 或改写 `task.yaml` 的 `updated_at`。
- **进展只追加与回执驱动确认**：`tasks/<task-id>/progress.md` 严格只追加写入。动作确认必须通过 `wb action confirm` 显式签发回执（`tasks/<task-id>/evidence/confirm-<action-id>.json`），校验 `--by`、`--source` 与匹配的 `--digest <scope_sha256>`；绝不从 `progress.md` 文本推断完成状态，范围变更或回执丢失会自动使既有确认失效。
- **自动维护索引**：`tasks/INDEX.md` 由 `wb` 在任务变更和工作台升级时自动生成，请勿手工编辑。

---

## 编译构建与快速上手

### 离线编译与测试

```bash
cargo build --offline
cargo build --release --offline
cargo test --offline
```

本地只读看板前端源码位于 `web/src/{index.html,app.js,styles.css}`，构建产物预置在 `src/static/` 并通过 `include_str!` 编入 `wb` 二进制。若修改了前端源码，可执行以下命令重新同步静态资源：

```bash
cd web && npm run build
```

### 初始化工作台

```bash
./target/release/wb init ~/my-workbench --yes --example --agent-guide
cd ~/my-workbench
```

常用 `wb init` 选项：

- `--dry-run`：仅预览将创建的目录与文件，不实际写入磁盘。
- `--name <名称>`：设置 `.wb.yml` 中的工作台显示名称。
- `--repos-root <路径>`：指定外部代码仓库根目录（默认 `../repos`）。
- `--timezone <时区>`：设置工作台时区（如 `Asia/Shanghai`）。
- `--port <端口>`：设置本地看板默认端口（默认 `8791`）。
- `--example`：生成示例任务 `TASK-EXAMPLE-001` 以便立即体验。
- `--agent-guide`：若当前目录无 `AGENTS.md`，生成指向 `WORKBENCH.md` 的协作导引。

### 工作台定位优先级

每次执行 `wb` 时，按以下顺序定位当前工作台根目录（包含 `.wb.yml`）：

1. 全局参数 `--workspace <路径>`
2. 环境变量 `WB_ROOT`
3. 从当前工作目录向上逐级查找 `.wb.yml`

---

## 核心工作流与 `wb` 子命令

### 1. 日常任务登记与秒级交接

```bash
# 创建任务并登记来源会话
wb new TASK-101 \
  --title "实现缓存失效策略" \
  --goal "解决并发写入场景下旧缓存读取问题" \
  --priority P1 --category requirement --owner alice \
  --session-id sess-1 --host devbox --session-title "[FD] 缓存需求讨论"

# 推进状态、追加时间顺序进展、维护下一步与阻塞项
wb status TASK-101 in_progress --note "开始编写核心逻辑"
wb progress TASK-101 "完成缓存双删逻辑初稿"
wb next TASK-101 add "补充并发竞态单元测试"
wb blocker TASK-101 add "等待测试环境配置文件确认"

# 新开会话输出 <= 80 行秒级交接简报
wb context TASK-101 --plain
```

更多任务维护命令：

- `wb context <id> [--plain | --full | --json] [--max-lines 80]`
- `wb next <id> add|done|remove|clear ...` 与 `wb blocker <id> add|resolve|remove|clear ...`
- `wb milestone <id> add|complete|done|reopen|undone|remove <title> [--done]`
- `wb component|doc|material <id> add|remove ...` 与 `wb link <id> <url>`
- `wb task list [--status ...] [--category ...] [--priority ...] [--owner ...] [--component ...] [--json]`
- `wb task stats [--json]`
- `wb task record [--task <id>] --session-id <sid> --title "<标题>" [--host localhost] [--category ...] [--role associated]`

**会话单前缀规则**：会话标题仅允许无前缀或单一合法前缀——`[FD]`（需求开发）、`[CR]`（代码评审）、`[OC]`（运维值班）或 `[DA]`（数据统计）；多前缀叠加或未知前缀将被拒绝（退出码 `2`）。

### 2. 交付声明、只读同步与动作确认回执

```bash
# 提交或更新 delivery.yaml（更新时可传 --expected-revision 做乐观并发校验）
wb delivery apply TASK-101 --file /path/to/delivery-draft.yaml

# 查看当前交付声明
wb delivery show TASK-101

# 只读采集外部 Git 仓库现状（无 Git 环境时优雅降级）
wb sync TASK-101          # 或同步全部任务：wb sync --all

# 固定 compare_base 为当前观察到的最新提交
wb delivery pin TASK-101

# 查看待确认动作及当前 scope_sha256 校验值
wb action list TASK-101

# 显式签发动作确认回执（必须提供 --by、--source 与匹配的 --digest）
wb action confirm TASK-101 act-test --by alice --source "local-cargo-test" --digest <scope_sha256>

# 导入与导出验证证据
wb evidence import TASK-101 ./test-report.log
wb evidence export TASK-101 --out ./exported-evidence
```

### 3. 八维工程评审准备、健康体检与安全升级

```bash
# 生成八维工程评审提示词，并自动派生登记三份一致交付产物路径
wb review prepare TASK-101 --url https://example.local/pr/42 --report analysis/cr-42.md

# 只读健康体检（配置、格式、文档引用、分支一致性、失效或缺失回执）
wb doctor --json

# 工作台升级（--dry-run 预览，--apply 先备份再升级）
wb workspace upgrade --dry-run
wb workspace upgrade --apply
```

`wb review prepare` 内置八维工程评审框架：功能与范围、问题与解决效果、兼容性、稳定性、性能与成本、异常防御、职责边界、验证充分性，并同步派生 `<report>`、`<stem>.summary.<ext>` 与 `<stem>.comments.<ext>` 路径。

---

## 本地只读网页看板与可选扩展

```bash
# 启动看板（仅绑定 127.0.0.1，默认读取 .wb.yml 的 dashboard_port: 8791）
wb dashboard serve --port 8791

# 查看状态与安全停止（停止前核对 .wb-runtime/dashboard.json 中的 instance_id）
wb dashboard status --json
wb dashboard stop

# 查看当前运行环境扩展清单及门控/探针状态
wb extensions list --json
```

### 看板视图与安全边界

内置单页只读看板提供 5 个视图（`工作总览`、`任务看板`、`会话目录`、`仓库与评审`、`进展动态`），全量采用 `textContent` 安全 DOM 渲染，并支持抽屉式任务详情、Markdown 纯文本预览弹窗与 CSV 导出（`/api/export.csv`）。

- **回环与同源防护**：严格绑定 `127.0.0.1`，校验 `Host`（`127.0.0.1` / `localhost`），启用 CSP（`default-src 'self'`）、`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`；`POST /api/refresh` 与 `POST /api/shutdown` 校验 `Origin` / `Referer` 与 `X-Wb-Csrf-Token`（通过 `GET /api/bootstrap` 获取）。
- **路径穿越与公式注入防御**：`/api/doc` 仅允许只读预览已在任务中登记的相对 `.md` 文件（拦截 `..`、反斜杠、符号链接逃逸与超过 `2 MiB` 的文件），并在输出前经 `src/dashboard/redaction.rs` 脱敏凭证；`/api/export.csv` 对 `=`、`+`、`-`、`@`、`\t`、`\r` 开头单元格进行公式转义。
- **采集体积上限**：单文件 `2 MiB`、单任务 `10 MiB`、工作台扫描 `100 MiB`、快照 JSON `25 MiB`；采集中途若监测到文件变更会自动中止更新并保留旧快照。

### 可选扩展发现与门控

从 `WB_EXTENSION_MANIFESTS` 或 `<workspace>/extensions/*.json` 发现的扩展清单，仅当在 `.wb.yml` 的 `extensions` 白名单中显式启用时才加载，并通过离线本地检查（`requires_bins`、`requires_paths`、`probe_cmd`）与单扩展异常隔离保证核心工作台稳定。

---

## 退出码与目录结构

| 退出码 | 含义 |
| --- | --- |
| `0` | 成功 |
| `1` | 本地网页看板运行错误（端口冲突、`instance_id` 不一致等） |
| `2` | 参数、数据格式、依赖环路、路径越界或 `wb doctor` 体检异常 |
| `3` | 工作台、任务、动作或目标文件不存在 |
| `4` | 乐观 `revision` 修订号冲突或 `scope_sha256` 范围校验值不匹配 |
| `5` | 本地文件读写 I/O 异常 |

```text
<workspace>/
├── .wb.yml                      # 工作台配置（schema、name、repos_root、timezone、dashboard_port、extensions）
├── WORKBENCH.md                 # wb init 生成的协作规范文档
├── tasks/
│   ├── INDEX.md                 # wb 自动生成的任务总索引（请勿手工编辑）
│   ├── session-types.json       # 去重会话分类台账
│   └── <task-id>/
│       ├── task.yaml            # 任务元数据、状态、里程碑、阻塞项、下一步与会话关联
│       ├── progress.md          # 只追加的时间顺序进展日志
│       ├── delivery.yaml        # 可选交付声明与动作范围定义
│       ├── delivery-history/    # 不可变 rev-XXXX.yaml 修订快照
│       └── evidence/            # confirm-<action-id>.json 确认回执与导入证据
├── docs/                        # 长期技术方案与设计说明
├── analysis/                    # 排查记录与 CR 评审产物
├── extensions/                  # 可选扩展清单（*.json）
└── .wb-runtime/                 # 文件锁、只读观察快照、升级备份与 dashboard.json（已加入 .gitignore）
```

---

## 详细文档

- [设计方案与字段规范](docs/DESIGN.md)
- [命令使用手册](docs/MANUAL.md)

---

## 许可证

MIT（见 [Cargo.toml](Cargo.toml) 声明）
