# Trestle（栈架 / `trestle` / `wb`）总体设计文档

## 一、系统边界与模块划分

Trestle（命令入口 `wb`）是一个本地优先、与具体业务领域无关的个人任务与交接管理工作台，将“任务登记/进展/方案/验证证据的工作台目录”与“外部业务源码仓库”严格分离。

| 模块路径 | 职责边界 |
| --- | --- |
| `src/main.rs` 与 `src/cli/mod.rs` | `wb` 命令入口、全局参数 `--workspace` 提取、子命令分发及统一退出码映射（0–5） |
| `src/core/errors.rs` | 统一退出码常量（`0..5`）与 `WbError` 错误类型 |
| `src/core/lock.rs` | 持久化文件锁（`task_lock` 使用 `tasks/<id>/.lock`，`workspace_lock` 使用 `.wb-runtime/locks/workspace.lock`），基于 `libc::flock` 与进程内互斥锁，锁文件不删除 |
| `src/core/yaml.rs` 与 `src/core/io_utils.rs` | 自包含 YAML 解析与序列化、同目录临时文件 + `sync_all` + `persist` 原子替换写入、JSON 读写、SHA256 计算、工作台路径边界检查 |
| `src/core/models.rs` | 状态/主类/优先级/动作类型枚举、会话单前缀校验、Git 分支合法性校验、`depends_on` 环路 DFS 检测、动作范围规范化 SHA256 计算 |
| `src/core/task_store.rs` | `task.yaml` CRUD、`progress.md` 只追加写入、`session-types.json` 去重台账、`INDEX.md` 自动生成 |
| `src/core/delivery.rs` | `delivery.yaml` 乐观修订号校验（冲突返回退出码 4）、先写 `delivery-history/rev-XXXX.yaml` 后替换 `delivery.yaml`、`compare_base` 固定 |
| `src/core/evidence.rs` | 动作确认回执（强制传入确认人、来源、范围校验值）、范围变化或回执丢失自动失效、证据导入导出；绝不从 `progress.md` 推断完成状态 |
| `src/core/sync.rs` | 只读采集外部 Git 仓库/worktree 的实际分支、完整 HEAD 提交、未保存修改数、文件 SHA256 与越界路径，不覆盖 `delivery.yaml` 或 `task.yaml`；兼容无 Git 环境 |
| `src/core/review.rs` | CR 八维工程评审规则（功能与范围、问题与解决效果、兼容性、稳定性、性能与成本、异常防御、职责边界、验证充分性）与三份一致交付物准备 |
| `src/core/context.rs` | 80 行以内的 `--plain` 秒级交接简报、`--full` 完整上下文与 `--json` 输出 |
| `src/workspace/` | 工作台定位优先级（`--workspace` > `WB_ROOT` > 上级 `.wb.yml`）、配置校验、`wb init` 非破坏接管、`wb doctor` 只读诊断、`wb workspace upgrade` 先备份后升级 |
| `src/dashboard/` | 凭证脱敏（`redaction.rs`）、带体积上限与变更保底的快照汇总（`collector.rs`）、纯标准库 `127.0.0.1` 只读 HTTP 服务（`server.rs`）与带 `instance_id` 校验的启停管理（`process.rs`） |
| `src/extensions/registry.rs` | 可选扩展清单发现（`WB_EXTENSION_MANIFESTS` / `<ws>/extensions/*.json`）与 `.wb.yml` 显式开关门控（未开启不加载、本地 `requires_bins`/`requires_paths` 无网探测、探针异常隔离） |
| `web/` 与 `src/static/` | 独立纯静态网页源码与构建脚本 `web/build.mjs`，编译期通过 `include_str!` 嵌入 `wb` 二进制，纯 `textContent` 安全 DOM 渲染 |

---

## 二、核心数据文件字段定义

### 1. `.wb.yml`
```yaml
schema: 1
name: my-workbench
repos_root: ../repos
timezone: Asia/Shanghai
dashboard_port: 8791
extensions: []
```

### 2. `tasks/<task-id>/task.yaml`
- `schema`: 整数（当前 `1`）
- `id`: 任务标识
- `title`: 任务标题
- `goal`: 任务目标说明
- `status`: `planning` | `in_progress` | `blocked` | `review` | `paused` | `done` | `cancelled`
- `priority`: `P0` | `P1` | `P2` | `P3`
- `owner`: 负责人
- `category`: `requirement` | `cr` | `oncall` | `analysis` | `unclassified`
- `components`: 去重活动标签列表（不记录工时）
- `milestones`: 里程碑列表（`title`, `done`, `updated_at`）
- `blockers`: 阻塞项列表
- `next_steps`: 下一步列表
- `docs`: 工作台内已登记文档路径列表
- `materials`: 工作台内已登记材料路径列表
- `links`: 外部链接（如 CR 链接）
- `sessions`: `{origin: {...} | null, associated: [...]}`（按 `(session_id, host)` 去重；单次会话 `category` 与任务主类相互独立）
- `created_at` / `updated_at`: UTC ISO-8601 时间戳

### 3. `tasks/<task-id>/delivery.yaml`
- `schema`: `1`
- `revision`: 整数（每次成功保存递增 1）
- `items`: 交付项列表，每项包含 `id`, `repo`, `worktree`, `source_branch`, `target_branch`, `compare_base`, `allowed_paths`, `depends_on`
- `actions`: 待完成动作列表，每项包含 `id`, `kind`（`auto_check` | `deploy_check` | `manual_review` | `manual_action`）, `title`, `item_id`, `scope`, `required`, `scope_sha256`

### 4. `.wb-runtime/observations/<task-id>.json`
- 由 `wb sync` 写入，包含 `synced_at`, `git_available`, `items`（含 `actual_branch`, `head_commit`, `dirty_file_count`, `modified_paths`, `file_sha256`, `branch_mismatch`, `out_of_scope_paths`）。

### 5. `tasks/<task-id>/evidence/confirm-<action-id>.json`
- 确认回执，包含 `task_id`, `action_id`, `confirmed_by`, `source`, `scope_sha256`, `delivery_revision`, `confirmed_at`, `note`。

---

## 三、`wb` 子命令与统一退出码

### 退出码定义
- `0`: 成功
- `1`: 本地网页程序运行错误（端口冲突、实例标识校验失败等）
- `2`: 参数或数据格式错误（非法状态/分支/会话前缀/环形依赖/路径越界/`doctor` 发现异常）
- `3`: 目标不存在或前置条件缺失（未找到工作台或目标任务/动作/文件）
- `4`: 修订号冲突或范围不一致（`delivery apply` 修订号冲突、`action confirm` 校验值不匹配）
- `5`: 本地文件读写异常

### 主要子命令
- 初始化与维护：`init`, `workspace upgrade`, `doctor`
- 任务与交接：`new`, `status`, `context`, `progress`, `next`, `blocker`, `milestone`, `component`, `doc`, `material`, `task list|record|stats`
- 交付与核查：`delivery show|apply|pin`, `action list|confirm`, `sync`, `evidence import|export`, `review prepare`
- 看板与扩展：`dashboard serve|status|stop`, `extensions list`

---

## 四、M1–M4 交付物与自动化测试映射

- **M1 (`tests/m1_core_and_cli.rs`)**: 锁文件保留与并发安全性、原子写入、会话前缀与去重、`delivery.yaml` 修订号冲突与环路校验、`action confirm` 范围失效与不读 `progress.md`、`wb sync` 只读观察与无 Git 降级、`wb context --plain` <= 80 行、`wb review prepare` 八维规则与三份产物。
- **M2 (`tests/m2_workspace_and_doctor.rs`)**: 工作台定位优先级（`--workspace` > `WB_ROOT` > 上级 `.wb.yml` > 退出码 3）、`wb init`（`--dry-run`, `--yes`, `--example`, `--agent-guide`, 重复初始化不覆盖）、`wb doctor` 只读核查、`wb workspace upgrade` 先备份后升级。
- **M3 (`tests/m3_dashboard_and_security.rs`)**: 快照采集体积上限（2MiB/10MiB/100MiB/25MiB）、采集中途变更保留旧快照、单任务故障隔离、敏感字段与 URL 凭证脱敏、`127.0.0.1` 绑定与端口冲突退出码 1、Host/Origin/CSRF 校验、`/api/doc` 路径穿越与符号链接逃逸拦截（403）、CSV 公式前缀转义、`wb dashboard stop` 实例标识核对与多工作台隔离。
- **M4 (`tests/m4_extensions_and_packaging.rs`)**: `wb extensions list` 未开启不加载、本地探针失败不加载、异常扩展隔离，以及独立 `wb` 二进制离线运行演练。
