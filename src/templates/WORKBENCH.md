# 通用个人工作台协作规范 (`WORKBENCH.md`)

本工作台是一个**本地优先、与具体业务领域无关**的任务与交接管理目录，面向人工协作者与编程助手。

## 一、核心原则

1. **目录与源码严格分离**：本目录仅保存任务登记、进展日志、交付声明、验证回执、长期方案与分析材料，禁止复制外部代码仓库的业务源码。
2. **计划声明与事实观察分离**：
   - `tasks/<task-id>/delivery.yaml` 保存显式声明的交付目标（仓库、检出目录、源分支、目标分支、允许修改路径、前置依赖 `depends_on`）与待完成动作。
   - `.wb-runtime/observations/<task-id>.json` 保存 `wb sync` 只读采集到的实际分支、完整提交记录、`SHA256` 校验值与未保存修改文件数；`wb sync` 绝不覆盖 `delivery.yaml`，也不修改任务的 `updated_at`。
3. **进展只追加、不改写**：`tasks/<task-id>/progress.md` 按时间顺序追加记录，禁止修改或删除既有历史条目。
4. **回执驱动的确认有效性**：人工评审与手工动作确认必须通过 `wb action confirm` 显式传入确认人、来源说明与动作范围 `scope_sha256`；禁止从 `progress.md` 的文本描述中推断检查已完成。当动作范围变更或 `evidence/confirm-<action-id>.json` 丢失时，原确认自动失效。
5. **自动索引禁止手工编辑**：`tasks/INDEX.md` 由 `wb` 自动生成，请勿手工修改。

## 二、目录约定

- `.wb.yml`：工作台配置（名称、外部代码根目录 `repos_root`、时区、看板端口、已启用扩展列表 `extensions`）。
- `tasks/<task-id>/task.yaml`：任务基本信息、状态、主类、标签、阻塞项、下一步与来源会话。
- `tasks/<task-id>/progress.md`：按时间顺序追加的进展日志。
- `tasks/<task-id>/delivery.yaml`：可选交付声明（受修订号乐观锁保护）。
- `tasks/<task-id>/delivery-history/`：历次已接受交付声明的只读历史快照。
- `tasks/<task-id>/evidence/`：动作确认回执与导入归档的验证证据。
- `docs/`：长期方案与技术说明文档。
- `analysis/`：排查记录、对比分析与代码评审产物。
- `.wb-runtime/`：运行时锁文件、观察快照、升级备份与看板快照（已被 `.gitignore` 忽略）。

## 三、任务分类与会话前缀约定

- 任务整体主类（`category`）与单次会话分类相互独立，可选值：`requirement`、`cr`、`oncall`、`analysis`、`unclassified`。
- 会话标题支持单一前缀标记，禁止叠加多个方括号前缀：
  - `[FD]`：需求开发（`requirement`）
  - `[CR]`：代码评审（`cr`）
  - `[OC]`：运维值班（`oncall`）
  - `[DA]`：数据统计（`analysis`）

## 四、新开会话秒级交接流程

1. 运行 `wb context <task-id> --plain` 获取 80 行以内的交接简报（目标、交付项、有效证据、全部阻塞与不一致项、最多 3 个下一步）。
2. 如需核对本地代码仓库现状，运行 `wb sync <task-id>` 更新只读观察记录。
3. 推进完成后，使用 `wb progress`、`wb next`、`wb status` 或 `wb action confirm` 更新工作台状态，并通过 `wb doctor` 核查一致性。
