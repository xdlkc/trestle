# 任务目录 (`tasks/`)

本目录保存工作台中的所有任务子目录（`tasks/<task-id>/`）、会话分类台账（`session-types.json`）与自动生成索引（`INDEX.md`）。

- **请勿手工编辑 `INDEX.md`**，该文件会在任务变更或执行 `wb workspace upgrade` 时自动刷新。
- 任务写操作统一通过 `wb` 命令完成，以确保文件锁与原子写入保护生效。
