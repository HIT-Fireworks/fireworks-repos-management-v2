# 薪火仓库管理

这是 HIT-Fireworks 的完整仓库管理工具。普通用户不需要会写代码，不需要安装 Python 或 Rust，也不需要输入 JSON、仓库编号、哈希或确认口令。

## 下载和启动

1. 在 GitHub Releases 下载 `fireworks-repository-manager-windows.zip`；
2. 完整解压 ZIP；
3. 双击 `启动薪火仓库管理.cmd`；
4. 使用 `↑` / `↓` 选择、`Enter` 确认、`Esc` 返回。

## 首页功能

- **从教务系统更新数据**：连接哈尔滨工业大学本部教务系统，按培养方案版本或执行教学计划年级、院系和完整专业身份查询；
- **查看和搜索资料**：按课程名或资料名浏览当前仓库；
- **管理远端仓库**：预览并同步 Registry、创建缺失仓库、修正描述/公开性/template/default branch、归档不再使用的空仓；
- **合并几份资料**：中文列表多选、填写结果名、预览后执行；
- **拆分一份资料**：按完整课程代码及名称、独立文件明确分配到目标仓库；共享文件关联的课程必须同仓；
- **查看任务记录**：恢复中断的教务更新、Registry/仓库同步、合并或拆分；
- **系统检查**：检查离线数据、Git 与 GitHub 登录状态；
- **退出**。

## 从教务系统更新数据

1. 选择“从教务系统更新数据”后，程序自动打开独立的 Microsoft Edge 或 Google Chrome 登录窗口；
2. 在该窗口完成哈尔滨工业大学统一身份认证。程序会自动检测登录结果，无需查看或复制 Cookie；
3. 登录窗口使用本机专用浏览器数据目录；Windows 下位于 `%LOCALAPPDATA%\FireworksRepositoryManager`。认证状态由 Edge/Chrome 管理并保留，不读取日常浏览器配置，也不另存明文 Cookie；
4. 选择培养方案或执行教学计划，再选择版本或年级、院系和专业。专业保留官方全名、专业代码和培养类型；程序默认通过备用地址 `http://jwts-hit-edu-cn.ivpn.hit.edu.cn:1080` 完整抓取所选范围；
5. 审阅课程新增、移出、教学字段及方案要求的变化，逐条选择“接受教务变化”或“保留当前数据”。按 `V` 查看新旧字段，按 `A` 全部接受，按 `R` 全部保留；
6. 审阅决定自动保存在本机，可从“查看任务记录”恢复，无需重新登录。每个接受的新增课程代码都需要明确选择现有资料库或新建独立课程资料库；
7. 查看本地仓库变更预览，再检查远端。选择和预览不会立即创建仓库或修改生产数据；
8. 最终确认后依次同步 Registry、管理远端仓库、验证远端，再原子切换本地三份状态文件。

认证失效时，程序会识别 401/403、登录重定向、ATrust 或统一身份认证页面，并通过专用学校登录窗口受控刷新认证；仍不可用时提示重新登录，不会把登录页当成课程数据。登录时可按 `Esc` 取消并返回，后台会关闭自己启动的浏览器进程，专用浏览器数据目录仍保留在本机。

## 数据与仓库同步

更新过程维护：

- `curriculum_plans`、`curriculum_records`、`course_descriptors`；
- `indexes/by-plan.json`、`indexes/pending-course-code.json`；
- `repository-manifest.json`、`repository-topology.v4.json`、`repository-file-routes.v4.json`；
- Registry 的 `curriculum/plans`、`records`、`descriptors` 和 `history` 动态树；
- GitHub 仓库的创建、description、公开性、archive、template 和默认分支。

每个完整课程代码直接绑定唯一 `repo_id` 与 `physical_repository_id`，不存在资源组中间层。既有课程代码保留仓库绑定。新代码由用户显式选择现有资料库或新建资料库；同名只作为建议，不会获得旧文件。同一新代码出现在多个方案时只需选择一次归属。无资料课程可以仅按代码拆仓。课程移出本次查询范围后保留历史记录和原有资料；未查询的方案保持不变。`course_groups` 与 `course_group_memberships` 仅表示教学计划导航关系，不是资源归属。

## 安全与恢复

- 操作前冻结本地状态、GitHub actor、Registry 和每个源/目标仓库的 exists/HEAD/tree；
- 计划和任务记录带内容身份，篡改后拒绝继续；
- Registry 先 clone、写固定动态树、commit、push 并校验；
- 仓库动作逐项记录，可在程序中断后恢复；
- 新仓库先保存 GitHub 不可变 ID，再等待模板初始提交和 README 就绪；中断后依据创建凭据续接，不重复创建或自动认领未知同名仓库；
- 更新课程 README 前核对冻结版本，写入时携带当前 SHA；连接中断后先检查远端结果，已经达到目标内容时不重复写入；
- 远端全部验证完成后，才原子切换本地 manifest/topology/routes；
- split/merge 使用精确 Git blob→tree→commit，保持实际相对路径和原字节；资料使用预设中文分类，软件包和多文件文档保留必要内部结构。同路径或文件/目录冲突拒绝操作，先明确重命名、重新清点再预览；
- 任何本地或远端漂移都会停止。
- 每个源课程代码及已清点文件恰好去往一个目标；共享文件不能复制或跨仓拆散，显式文件选择不能违背课程归属；
- 预览读取冻结源树，未清点文件保留或明确随迁，无法确定去向就拒绝；仓库 README、LICENSE、repository.toml、workflow 不会被空树静默删除；
- split/merge 同步更新 descriptor、教学记录、仓库清单与课程路由，并通过同一个本地状态事务落盘，后续教务更新不恢复旧归属。

## Windows 发行包

普通用户包只包含：

```text
薪火仓库管理.exe
启动薪火仓库管理.cmd
请先看我.txt
config/repository-topology.v4.json
config/repository-file-routes.v4.json
data/repository-manifest.no-collection.v4.json
data/.fireworks-json/
config/.fireworks-json/
```

生产运行时是单一 Rust EXE；登录功能使用系统已安装的 Microsoft Edge 或 Google Chrome，不会读取日常浏览器配置。Python 不进入发行包。`scripts/fireworks_manager_core.py` 与 `scripts/repository_management.py` 已撤下命令行入口，函数仅供旧 fixture 的历史离线行为对照；不得导入这些函数操作当前 canonical 数据。`generate-repository-manifest.py`、`provision-repositories.py`、`validate-repository-manifest.py` 以及旧资源组配置是冻结迁移执行器/历史证据，不是当前生成、校验或生产管理入口。当前控制面仅使用上述 Rust 运行时和三份 canonical 文件（含其分片）。

全量教务数据使用内容寻址 JSON 分片保存，程序读取时会核验每片的 SHA-256 和字节数并还原完整数据。安装包中的 `.fireworks-json` 目录如存在，必须与对应 JSON 文件一起保留；只复制三份根 JSON 会导致数据不完整。

大规模预览校验采用借用式序列化和逐分片规范哈希，保持原有冻结身份不变，不为校验复制整份数据树。原始 GitHub 错误及退出状态保存在任务记录中，便于区分认证、参数和网络错误。

## 当前快照与分类规则

- 176 个受控仓库，其中 117 个有资料仓；
- 4,613 份培养方案及执行教学计划，分别保留方案版本和入学年级；
- 226,560 条原始记录、16,431 个独立课程代码；
- 3,857 条文件路由，对应 9,159,380,782 字节原始资料。

使用 `教材/`、`笔记/`、`课件/`、`试卷/`、`作业/`、`实验/`、`软件/`、`教程/`、`模板/`、`项目/`、`其他/` 预设中文分类。维护者不得自行新增根级分类；需要新分类时须统一修改管理规则。根目录只保留仓库说明、许可证及配置。分类不是资源组，不绑定课程集合；真实软件包和多文件文档可在分类内保留内部目录。

## CI 与发行

资料与模板仓不独立启动 CI；Registry 集中验证分片与路由。本仓 `native-ci.yml` 仅在 Rust 代码等相关 PR 或手动运行时检查；Windows 包仅由 `manager-v*` Tag 或手动打包，包含全部引用的隐藏 JSON 分片。工作流使用缓存、超时与并发控制，避免每次资料变更都编译管理工具。

## 维护人员验证

```sh
cargo test --locked --manifest-path repository-tui/Cargo.toml
cargo run --quiet --locked --manifest-path repository-tui/Cargo.toml -- --check
python scripts/validate-registry.py --root .
```


Rust 测试覆盖两类教务 HTTP 请求、完整分页和查询范围、官方专业全名与培养类型、教学模块与毕业要求、认证失效刷新、Cookie 请求头、重复及无代码课程差异、审阅接受/保留、新课程显式归属、历史记录和旧文件保护、审阅跨重启恢复、篡改与状态漂移拒绝、Registry bare-remote、仓库生命周期、中文八项首页和 split/merge 状态机。
