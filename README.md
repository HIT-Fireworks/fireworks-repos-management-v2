# 薪火课程仓库管理 v2

`fireworks-repos-management-v2` 是 HIT-Fireworks 的唯一仓库管理产品。它将人类 TUI、Agent Skill 和 JSON 核心接到同一套 v4 状态与安全状态机；TUI、Skill 都不直接改写 manifest、topology、routes，也不绕过计划调用 GitHub mutation。

## 架构与权威数据

```text
Rust ratatui TUI ─┐
Agent Skill      ├─ JSON CommandEnvelope → Python JSON Core → 状态引擎 / GitHub
CLI              ┘
```

默认工作区包含：

```text
data/repository-manifest.no-collection.v4.json
data/repository-management-operations/       # 计划与 journal；可不存在
config/repository-topology.v4.json
config/repository-file-routes.v4.json
scripts/fireworks_manager_core.py
scripts/repository_management.py
```

`--workspace PATH` 只指定数据工作区；核心脚本由 TUI 从 `FIREWORKS_MANAGER_CORE`、开发树 `CARGO_MANIFEST_DIR` 或可执行文件祖先目录定位，不要求外部数据目录复制 `scripts/`。

当前附带快照运行时核对得到：100 个仓库、2,618 条课程代码路由、3,857 条文件路由、75 个完整库存仓库、16 个虚拟集合、18 个 special-topic 路由。数字不硬编码在适配器中。

## JSON 核心

直接查询：

```sh
python scripts/fireworks_manager_core.py query inspect
python scripts/fireworks_manager_core.py query validate
python scripts/fireworks_manager_core.py query search 数理逻辑 --limit 50
python scripts/fireworks_manager_core.py query repository COURSES-RA-ED4F15651BB9
```

通用 transport 从 stdin 读取一个 `CommandEnvelope`：

```sh
python scripts/fireworks_manager_core.py --workspace . invoke < request.json
```

支持的 command：

| family | kind | 作用 |
|---|---|---|
| `query` | `inspect` / `validate` | 健康、状态 identity |
| `query` | `search` / `repository` | 按 repo_id、语义、课程代码、原始课程名定位 |
| `query` | `routes` | 文件与课程代码路由；`limit=0` 明确表示返回完整快照，省略 limit 默认 200 |
| `query` | `plan` | 列出或读取已持久化计划；无 operations 目录返回空列表 |
| `query` | `journals` | 列出 journal 及 `recovery_state`：`completed`、`resumable`、`drifted` 或 `invalid` |
| `plan` | `split` | 以完整互斥资源组和显式路径分配生成冻结拆分计划 |
| `plan` | `merge` | 生成整仓合并计划；冲突路径自动记录 relocation |
| `execute` | `apply` | 仅执行已签名、未漂移且明确批准的计划 |
| `execute` | `verify` | 仅验证 completed journal 的最终 topology/routes 与远端 HEAD |
| `execute` | `resume` | 仅按 journal 的 `resumable` 状态续跑 |

### 计划与远端基线

`plan.split` 参数至少包括：

```json
{
  "source_repo_id": "COURSE-A",
  "targets": [
    {"repo_id":"COURSE-A1","display_name":"课程一","resource_group_ids":["group-a"],"paths":["README.md"]},
    {"repo_id":"COURSE-A2","display_name":"课程二","resource_group_ids":["group-b"],"paths":[]}
  ]
}
```

`plan.merge` 参数包括 `source_repo_ids`、`target_repo_id` 和 `display_name`。计划持久化前冻结：

- `plan_identity_sha256`（从持久化计划内容重算，排除 `created_at` 与 identity 自身）；
- organization、请求 actor、workspace identity；
- Registry manifest/baseline；
- 源仓库 commit/tree、目标仓库存在性与 HEAD、remote URL。

计划的 `after` 内容不可变。远端目标 HEAD 解析写入 journal 的 `resolved_after_routes`，不回写计划。

### 执行状态机

```text
read_only → frozen_plan → reviewed → confirmed → applying → verifying → completed
                                      ├──────────────→ failed → resumable
                                      └──────────────→ drifted
```

所有 APPLY/RESUME challenge 必须原样回传；大小写、尾随空格和改写都拒绝。apply 会在 Git 迁移前检查所有 baseline，并对缺失目标执行已记录的幂等建仓。任何 identity、actor、Registry、HEAD、目标存在性或工作区状态漂移都停止。

## Rust TUI

依赖 Rust、ratatui、crossterm。启动：

```sh
cargo run --locked --manifest-path repository-tui/Cargo.toml -- --workspace .
```

主工作台通过 `Tab` / `Shift-Tab` 切换四页：

- **Repositories**：搜索、语义详情、课程代码/原始课程名、文件库存；`Space` 多选合并源仓库；
- **Routes**：浏览文件路由与课程代码路由；
- **Plans**：查看 split/merge 冻结计划、源/目标、文件移动、风险、完整 identity 和 confirmation；
- **Journals**：查看 status 与 `recovery_state`；仅 `resumable` journal 可输入 `RESUME` challenge，只有 `completed` journal 可 verify。

计划命令面板：

- `p` → `m`：用 Space 选中的仓库进入 merge 向导，填写目标 repo_id 与展示名；
- `p` → `s`：对当前完整库存仓库填写完整 split 分区（紧凑格式或 `SplitTarget` JSON 数组）；
- 计划详情 `a`：输入核心返回的精确 `APPLY <operation-id>`；
- journal 详情 `r`：输入精确 `RESUME <operation-id>`；`v`：验证 completed journal。

通用按键：`j/k` 或方向键移动，`Enter` 进入详情，`/` 搜索，`h` 健康，`r` 刷新，`?` 帮助，`Esc/b` 返回，`q` 退出。窄屏自动退化为单栏；中文截断按显示宽度处理。

无头烟测：

```sh
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --workspace . --check
```

Rust `TestBackend` 回归测试覆盖 ASCII/中文输入后的重绘、四页切换、split 规格解析、严格 challenge 和 Unicode 显示宽度。Windows 实际 PTY 已验证可进入 raw mode 并绘制总览；若自动化终端无法注入后续按键，不将未观测的端到端序列声明为通过。

## Agent Skill

Skill 位于 `skills/hit-fireworks-repository-management/`，是 JSON 核心的薄适配器：

1. `query.inspect` → 保存 workspace identity；
2. `query.search` / `query.repository` / `query.routes` 定位目标；
3. `plan.split` 或 `plan.merge` 生成并保存计划；
4. 向用户展示完整风险、源/目标、路由、文件移动、远端 baseline、plan identity 和 confirmation；
5. 仅在用户批准当前完整 identity 后 `execute.apply`；
6. `execute.verify` 闭合最终状态；
7. `query.journals` 判断 `completed` / `resumable` / `drifted` / `invalid`，仅按参考流程 resume。

参考文件：

- `references/schema.md`：Envelope、结果字段、参数和错误码；
- `references/mutation.md`：apply/verify 的确认与停止门禁；
- `references/recovery.md`：journal 恢复、远端重查和漂移处理。

## 安全边界

默认所有 query 只读，不创建 operations 目录、不写权威状态文件。远端 mutation 只由核心 execute 路径执行；TUI/Skill 不直接调用 GitHub API。不存在的 operations 目录在 query.plan/query.journals 中等同于空集合。

不得使用 `--yes`、修改字段后重试、用简称代替 identity、把 CourseGroup 当作仓库合并指令，或在计划外自动重映射。

## 验证

本地等价 CI 命令：

```sh
python -m py_compile scripts/*.py tests/*.py
python -m unittest tests/test_fireworks_manager_core.py
python -m unittest tests/test_fireworks_manager_state_machine.py
python -m unittest tests/test_repository_management.py
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --workspace . --check
```

CI 会运行同一组检查；测试不执行真实 GitHub mutation，远端写操作必须由用户批准的计划显式触发。
