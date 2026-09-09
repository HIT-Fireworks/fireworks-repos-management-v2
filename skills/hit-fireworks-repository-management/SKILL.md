---
name: hit-fireworks-repository-management
description: 管理 HIT-Fireworks 完整仓库生命周期，包括从 HIT 本部教务系统更新课程数据、审阅差异、重建 Registry/manifest/topology/routes、创建与同步 GitHub 仓库、归档和恢复。普通用户必须优先使用 Windows 中文向导，不暴露代码、JSON、内部 ID 或哈希。
---

# HIT-Fireworks 仓库管理

## 普通用户入口

默认提供 GitHub Release 中的 `fireworks-repository-manager-windows.zip`：

1. 完整解压；
2. 双击 `启动薪火仓库管理.cmd`；
3. 使用 `↑↓`、`Enter`、`Esc`。

首页必须包含：

- 从教务系统更新数据；
- 查看和搜索资料；
- 管理远端仓库；
- 合并几份资料；
- 拆分一份资料；
- 查看任务记录；
- 系统检查；
- 退出。

不得要求普通用户安装 Python/Rust、运行命令、输入 JSON、repo_id、identity、commit/tree 或 APPLY/RESUME challenge。

## 教务更新契约

生产运行时默认使用 `http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080`：

1. Cookie 仅存在 `UpdateSession` 内存，离开更新向导即释放，不写盘、不进日志；
2. GET `/pyfa/queryPykc` 读取年级和院系；
3. POST `/pub/queryYxzyList_bbh` 读取专业；
4. POST `/pyfa/queryPykc` 分页抓取课程；
5. 识别 401/403、登录重定向、ATrust 和统一身份认证页面；
6. staging 与生产状态同盘，完整校验后才生成差异；
7. duplicate 和无代码课程按 occurrence 对齐，不折叠；
8. 每项变化选择接受或保留现状，支持批量；
9. 物化后增量重建 plans/records/descriptors/indexes/topology/routes；
10. 先同步 Registry 和远端仓库并验证，最后原子切换本地三文件。

## 增量映射规则

- 完整课程代码直接且唯一绑定 physical repository 与 repo，保留已有绑定；
- 新代码必须显式选择现有仓库或新建仓库，同名只作为审阅建议，不自动获得旧文件；
- 仓库是唯一资源边界；拆分完整分配课程代码与文件，共享文件关联代码必须同仓；
- 普通独立资料位于根目录，软件包/多文件文档保留真实结构；同路径冲突拒绝，不生成资源组或占位目录；
- 移除代码不删除有资料仓；仅无文件且不再承载课程的仓进入归档预览；
- course_code_routes、curriculum records/descriptors/indexes 必须全量一致。

## Registry 与远端仓库生命周期

Registry 固定动态树：

```text
repository-manifest.json
repository-topology.v4.json
repository-file-routes.v4.json
curriculum/plans/<encoded>.json
curriculum/records/<record-id>.json
curriculum/descriptors/<encoded>.json
indexes/by-plan.json
indexes/pending-course-code.json
```

执行顺序：校验预览 → 写 update journal/bundle → clone Registry → 清理并写动态树 → commit/push/verify → 仓库 create/update/archive/unarchive → description/visibility/template/default branch 校验 → 最终 verify → 本地三文件原子切换 → completed。

每次计划冻结 workspace、GitHub actor、Registry、源/目标仓 exists/head/tree 和 remote URL。任务记录必须可跨重启恢复；漂移、篡改或目标非空时停止。

## Agent 与维护人员

生产运行时为单一 Rust `Manager`。Python 管理 CLI 已撤下，生成/迁移脚本仅供冻结历史离线审计，不适用于当前 canonical 数据。不得绕过 Manager 直接改 manifest/topology/routes 或调用 GitHub mutation。`references/schema.md` 与 `references/mutation.md` 仅记录已退役 Python 协议，不是现行 API。

关键接口：

- `begin_curriculum_update` / `update_majors` / `stage_curriculum_update`；
- `UpdateSession::set_decision` / `accept_all` / `reject_all`；
- `materialize_curriculum_update`；
- `plan_remote_sync` / `execute_remote_sync` / `verify_remote_sync`；
- `update_journals` 与跨重启恢复；
- split/merge plan/apply/resume/verify。
- `SplitTarget { repo_id, display_name, course_codes, paths }`：每个代码恰好分配一次，无码文件显式指定路径，跨代码共享约束不能被显式路径覆盖；
- split/merge 冻结并保留未清点源文件；无法确定去向或目标同名时拒绝。三份本地状态同步更新直接归属，history 保留原证据。

## 发行与验证

Windows ZIP 只能包含单一 Rust EXE、`启动薪火仓库管理.cmd`、说明和三份数据文件；不得包含 Python。

必须通过：

```sh
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --check
```

Rust 测试必须覆盖模拟教务 HTTP 完整请求链、Cookie/认证、分页、差异决策、增量重建、Registry bare-remote、仓库生命周期、update journal 恢复、中文八项首页和 split/merge 状态机。
