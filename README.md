# 薪火资料管理

这是给普通人使用的 HIT-Fireworks 资料管理工具。**不需要会写代码，不需要安装 Python 或 Rust，也不需要输入 JSON、仓库编号、哈希值或确认口令。**

## 最简单的使用方法

1. 在 GitHub Releases 下载 `薪火资料管理-Windows.zip`。
2. 完整解压 ZIP。
3. 双击 `启动薪火资料管理.cmd`。
4. 只使用：
   - `↑` / `↓`：选择；
   - `Enter`：确认；
   - `Esc`：返回。

首页提供五件事：

- 查看和搜索资料；
- 合并几份资料；
- 拆分一份资料；
- 查看任务记录；
- 系统检查。

查看资料完全离线可用。合并或拆分资料时，系统检查页会用中文提示是否需要安装 Git 或登录 GitHub。

## 普通用户会看到什么

### 查看资料

按课程名或资料名称搜索，查看课程名称、文件数量、占用空间和清点状态。界面以中文名称为主，不显示内部仓库编号。

### 合并资料

1. 在中文资料列表中按 Enter 勾选至少两份；
2. 填写合并后的中文名称；
3. 查看“会发生什么”的自然语言预览；
4. 选择“确认执行”。

同名文件不会覆盖，工具会自动放入独立目录。内部目标编号、计划身份和安全确认由程序生成并处理，不要求用户理解或抄写。

### 拆分资料

1. 选择一份已完成文件清点、包含多个独立内容组的资料；
2. 选择拆成几份；
3. 用左右方向键给每个中文课程组或零散文件选择去向；
4. 给每份结果填写中文名称；
5. 查看预览并确认。

工具会自动生成内部目标编号、完整分区、文件移动和课程路由，不要求输入 JSON 或资源组 ID。

### 任务记录

程序中断或网络失败后，首页进入“任务记录”。如果可以安全继续，界面显示“可以继续”；按 Enter 即可恢复。已完成任务可以再次检查最终结果。资料已变化或任务记录被修改时，程序拒绝自动继续并提示联系维护人员。

## 安全设计

人类界面隐藏技术细节，但不会降低安全门禁：

- 操作前冻结本地 manifest/topology/routes 身份；
- 校验源仓库和目标仓库远端版本；
- 计划内容带 SHA-256 身份，任务记录被修改会拒绝；
- 文件使用精确 Git tree 构造，不覆盖同名内容；
- 最终远端 HEAD 写入独立的 `resolved_after_routes`，原计划保持不变；
- topology 和 routes 分阶段原子写入，可在中断后继续；
- 任何本地或远端漂移都会停止。

## 单一 Rust 运行时

发行包只包含：

```text
薪火资料管理.exe
启动薪火资料管理.cmd
请先看我.txt
config/repository-topology.v4.json
config/repository-file-routes.v4.json
data/repository-manifest.no-collection.v4.json
```

生产运行时是单一 Rust 可执行文件，不启动 Python 子进程。程序自动从可执行文件同目录发现数据。

Python 文件仍保留在源码仓中，仅用于：

- 离线生成和审计 manifest；
- 验证历史迁移；
- 作为迁移到 Rust 期间的行为对照。

它们不进入 Windows 普通用户发行包。

## 当前数据

随发行包附带的 v4 快照包含：

- 100 个资料仓库；
- 2,618 条课程代码路由；
- 3,857 条文件路由；
- 75 个已完成文件清点的仓库；
- 16 个虚拟集合；
- 18 条 special-topic 路由。

## 维护人员开发

```sh
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --check
```

可选 `--workspace PATH` 仅供测试外部数据目录；普通用户无需任何参数。

Python 行为对照：

```sh
python -m py_compile scripts/*.py tests/*.py
python -m unittest tests/test_fireworks_manager_core.py
python -m unittest tests/test_fireworks_manager_state_machine.py tests/test_repository_management.py
```

Rust 测试覆盖中文首页、纯方向键向导、内部标识隐藏、语义拆分、自动目标编号、原生 bare-remote Git 迁移、课程路由更新、journal 防篡改、恢复和最终验证。

## Agent Skill

`skills/hit-fireworks-repository-management/` 仍供 Agent 使用。Agent 可以读取技术 identity 和 journal，但人类 TUI 不展示这些字段。两者使用相同 v4 状态和安全不变量。
