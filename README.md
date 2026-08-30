# 薪火仓库管理

这是 HIT-Fireworks 的完整仓库管理工具。普通用户不需要会写代码，不需要安装 Python 或 Rust，也不需要输入 JSON、仓库编号、哈希或确认口令。

## 下载和启动

1. 在 GitHub Releases 下载 `fireworks-repository-manager-windows.zip`；
2. 完整解压 ZIP；
3. 双击 `启动薪火仓库管理.cmd`；
4. 使用 `↑` / `↓` 选择、`Enter` 确认、`Esc` 返回。

## 首页功能

- **从教务系统更新数据**：连接哈尔滨工业大学本部教务系统，选择年级、院系和专业，抓取培养方案与课程；
- **查看和搜索资料**：按课程名或资料名浏览当前仓库；
- **管理远端仓库**：预览并同步 Registry、创建缺失仓库、修正描述/公开性/template/default branch、归档不再使用的空仓；
- **合并几份资料**：中文列表多选、填写结果名、预览后执行；
- **拆分一份资料**：把中文课程组和零散文件分配到若干目标；
- **查看任务记录**：恢复中断的教务更新、Registry/仓库同步、合并或拆分；
- **系统检查**：检查离线数据、Git 与 GitHub 登录状态；
- **退出**。

## 从教务系统更新数据

1. 选择“从教务系统更新数据”后，程序自动打开独立的 Microsoft Edge 或 Google Chrome 登录窗口；
2. 在该窗口完成哈尔滨工业大学统一身份认证。程序会自动检测登录结果，无需查看或复制 Cookie；
3. 登录窗口使用一次性临时数据目录，关闭向导后销毁；Cookie 只在当前进程内存中使用，不写入磁盘；
4. 选择年级、院系和专业；程序默认通过备用地址 `http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080` 分页抓取课程，并先写入同盘 staging；
5. 对新增、删除和修改逐条选择“接受教务变化”或“保留当前数据”，也可批量处理；
6. 程序物化选择并增量重建培养方案、课程记录、课程描述、索引、拓扑和课程路由；
7. 查看自然语言仓库变更预览；
8. 确认后依次同步 Registry、管理远端仓库、验证远端，再原子切换本地三份状态文件。

认证失效时，程序会识别 401/403、登录重定向、ATrust 或统一身份认证页面，并用中文要求重新登录；不会把登录页当成课程数据。登录时可按 `Esc` 立即取消并返回，后台会关闭独立浏览器会话。

## 数据与仓库同步

更新过程维护：

- `curriculum_plans`、`curriculum_records`、`course_descriptors`；
- `indexes/by-plan.json`、`indexes/pending-course-code.json`；
- `repository-manifest.json`、`repository-topology.v4.json`、`repository-file-routes.v4.json`；
- Registry 的 `curriculum/plans`、`records`、`descriptors` 动态树；
- GitHub 仓库的创建、description、公开性、archive、template 和默认分支。

既有课程代码保留资源组、物理仓和 repo 绑定；新代码优先复用同名资源组，否则创建稳定资源组并映射学院无资料仓。课程从教务系统消失时，不删除有资料仓；只有无文件且不再承载课程的仓库才进入归档预览。

## 安全与恢复

- 操作前冻结本地状态、GitHub actor、Registry 和每个源/目标仓库的 exists/HEAD/tree；
- 计划和任务记录带内容身份，篡改后拒绝继续；
- Registry 先 clone、写固定动态树、commit、push 并校验；
- 仓库动作逐项记录，可在程序中断后恢复；
- 远端全部验证完成后，才原子切换本地 manifest/topology/routes；
- split/merge 使用精确 Git blob→tree→commit，不静默覆盖同名文件；
- 任何本地或远端漂移都会停止。

## Windows 发行包

普通用户包只包含：

```text
薪火仓库管理.exe
启动薪火仓库管理.cmd
请先看我.txt
config/repository-topology.v4.json
config/repository-file-routes.v4.json
data/repository-manifest.no-collection.v4.json
```

生产运行时是单一 Rust EXE；登录功能使用系统已安装的 Microsoft Edge 或 Google Chrome，不会读取日常浏览器配置。Python 仅保留在源码仓中，用于离线生成、历史审计和行为对照，不进入发行包。

## 当前快照

- 100 个仓库；
- 211 个培养方案；
- 10,468 条课程记录；
- 2,618 条课程代码路由；
- 3,857 条文件路由；
- 75 个已完成文件清点的仓库。

## 维护人员验证

```sh
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --check
python -m py_compile scripts/*.py tests/*.py
python -m unittest tests/test_fireworks_manager_core.py
python -m unittest tests/test_fireworks_manager_state_machine.py tests/test_repository_management.py
```

Rust 测试覆盖模拟教务 HTTP 完整请求链、认证失效、200+1 分页、Cookie 请求头、duplicate/无代码 occurrence 差异、accept/reject、增量重建、Registry bare-remote、仓库生命周期、update journal 跨重启恢复、中文八项首页和 split/merge 状态机。
