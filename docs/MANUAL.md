# 通用个人工作台使用手册 (`MANUAL.md`)

## 一、构建与快速上手

### 1. 构建二进制
```bash
cargo build --release --offline
# 生成可执行文件 ./target/release/wb
```

### 2. 初始化工作台
```bash
wb init ~/my-workbench --yes --example --agent-guide
cd ~/my-workbench
```
常用选项：
- `--dry-run`：仅预览将创建的目录与文件，不实际写入磁盘。
- `--name <名称>`：设置工作台显示名称。
- `--repos-root <路径>`：指定外部代码仓库根目录（默认 `../repos`）。
- `--port <端口>`：设置本地看板默认端口（默认 `8791`）。
- `--example`：生成示例任务 `TASK-EXAMPLE-001` 以便立即体验。
- `--agent-guide`：若当前目录无 `AGENTS.md`，创建一个指向 `WORKBENCH.md` 的协作导引。

### 3. 工作台定位规则
`wb` 按以下顺序定位当前工作台根目录（含 `.wb.yml`）：
1. 命令行 `--workspace <路径>`
2. 环境变量 `WB_ROOT`
3. 从当前工作目录向上逐级查找 `.wb.yml`

---

## 二、日常任务登记与交接

```bash
# 1. 创建任务并登记来源会话
wb new TASK-101 --title "实现缓存失效策略" --goal "解决并发写入场景下旧缓存读取问题" \
  --priority P1 --category requirement --owner alice \
  --session-id sess-1 --session-host devbox --session-title "[FD] 缓存需求讨论"

# 2. 推进状态、追加进展、维护下一步与阻塞项
wb status TASK-101 in_progress --note "开始编写核心逻辑"
wb progress TASK-101 "完成缓存双删逻辑初稿"
wb next TASK-101 add "补充并发竞态单元测试"
wb blocker TASK-101 add "等待测试环境配置文件确认"

# 3. 新开会话秒级交接（默认输出 <= 80 行）
wb context TASK-101 --plain
```

### 会话分类与单前缀规则
```bash
wb task record --task-id TASK-101 --session-id sess-2 --host laptop \
  --title "[CR] 缓存策略评审跟进" --role associated
```
仅允许无前缀或单一合法前缀 `[FD]`、`[CR]`、`[OC]`、`[DA]`；多前缀叠加或未知前缀会被参数校验拒绝（退出码 `2`）。

---

## 三、交付声明、仓库同步与动作确认

```bash
# 提交或更新 delivery.yaml（受修订号保护，修改时须传 --expected-revision）
wb delivery apply TASK-101 --file /path/to/delivery-draft.yaml

# 只读采集外部 Git 仓库现状（不改写 delivery.yaml 或 task.yaml 的 updated_at）
wb sync TASK-101

# 固定 compare_base 为当前观察到的最新提交
wb delivery pin TASK-101

# 查看动作列表及其当前 scope_sha256
wb action list TASK-101

# 显式签发确认回执（必须同时提供 --by、--source 与匹配的 --digest）
wb action confirm TASK-101 act-test --by alice --source "local-cargo-test" --digest <scope_sha256>

# 导入与导出验证证据
wb evidence import TASK-101 ./test-report.log
wb evidence export TASK-101 --out ./exported-evidence
```

---

## 四、代码评审准备、健康检查与工作台升级

```bash
# 生成八维工程评审提示词，并登记三份结论一致的输出路径
wb review prepare TASK-101 --url https://example.local/pr/42 --report analysis/cr-42.md

# 只读健康检查（配置、格式、引用路径、分支一致性、失效/缺失回执）
wb doctor --json

# 工作台升级（--dry-run 预览，--apply 先备份再升级）
wb workspace upgrade --dry-run
wb workspace upgrade --apply
```

---

## 五、本地只读网页看板与可选扩展

```bash
# 启动看板（仅绑定 127.0.0.1）
wb dashboard serve --port 8791

# 查看状态与安全停止（停止前会先核对 instance_id）
wb dashboard status --json
wb dashboard stop

# 查看当前运行环境扩展清单及启停探测状态
wb extensions list --json
```
