---
name: hit-fireworks-repository-management
description: 管理 HIT-Fireworks 资料仓库。普通用户请求时必须优先提供 Windows 易用版下载、双击启动和中文向导，不让用户接触代码、JSON、repo_id、identity 或 challenge；Agent/维护任务才使用技术状态与离线 Python 行为对照。
---

# HIT-Fireworks 资料管理

## 先判断使用者

### 普通用户

默认使用 GitHub Release 中的 `薪火资料管理-Windows.zip`：

1. 下载并完整解压；
2. 双击 `启动薪火资料管理.cmd`；
3. 使用 `↑↓`、`Enter`、`Esc`。

不要要求普通用户安装 Python/Rust、运行命令、填写 JSON、repo_id、资源组 ID、plan identity 或 APPLY/RESUME challenge。中文向导内部完成目标编号、计划签名、远端基线与安全确认。

首页入口：查看和搜索资料、合并几份资料、拆分一份资料、查看任务记录、系统检查。

### Agent 或维护人员

生产运行时是 `repository-tui` 中的原生 Rust `Manager`，直接读取：

```text
data/repository-manifest.no-collection.v4.json
config/repository-topology.v4.json
config/repository-file-routes.v4.json
data/repository-management-operations/   # 可不存在
```

Rust Manager 负责 inspect/search/detail/split-options/plan/apply/resume/verify、Git tree、远端基线和 journal。不得绕过 Manager 直接修改 topology/routes 或调用 GitHub mutation。

Python 脚本只用于离线 manifest 生成、历史审计和行为对照；不进入普通用户发行包，不是生产运行时依赖。

## 人类向导契约

- 合并：中文列表勾选至少两份 → 填结果中文名 → 自然语言预览 → 确认执行。
- 拆分：选择资料 → 选择数量 → 逐项给中文课程组/零散文件选去向 → 填中文名 → 预览 → 确认。
- 任务记录：仅 `resumable` 显示“可以继续”，completed 可检查结果，drifted/invalid 停止。
- 系统检查：浏览始终离线可用；变更时自然语言提示 Git/GitHub 状态。

界面不得展示 repo_id、JSON、identity、journal、APPLY、RESUME、commit/tree 或错误码。技术信息只存于内部计划、任务记录和 Agent 审计结果。

## 安全不变量

Rust Manager 必须同时保证：

- topology/routes schema、generation、仓库和路径唯一性有效；
- split 源库存完整，课程组完整互斥，跨组/零散文件显式分配，目标非空；
- merge 至少两个完整源，同名路径 relocation，不静默覆盖；
- 文件路由与 course_code_routes 同步迁移；
- 计划绑定 workspace identity、GitHub actor、Registry、源/目标 exists/head/tree 和 remote URL；
- plan identity 可从内容重算；计划不可变；
- journal 校验 operation/kind/status、remote URL、expected HEAD、resolved routes hash、Git target status/commit；
- 新目标仓可幂等创建，已创建空仓的中断可恢复，非空目标不得覆盖；
- Git 以精确 blob→tree→commit 构造并核对远端 main；
- topology 后 routes 分阶段原子切换；漂移即停止。

## 发行验证

Windows ZIP 只能包含：

```text
薪火资料管理.exe
启动薪火资料管理.cmd
请先看我.txt
config/repository-topology.v4.json
config/repository-file-routes.v4.json
data/repository-manifest.no-collection.v4.json
```

不得包含 `.py`、`.pyc`、scripts 或 Python 运行时。

必须运行：

```sh
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --check
```

Rust 测试应覆盖中文首页、纯方向键路径、技术标识隐藏、真实 v4 语义拆分、自动目标 ID、bare-remote split/merge、课程路由、actor/Registry/tree baseline、目标建仓、空仓恢复、journal 防篡改和 verify。

技术字段与旧 JSON 行为对照见 references；普通用户沟通不要引用这些文件内容。
