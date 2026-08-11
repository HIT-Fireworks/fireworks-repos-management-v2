# HIT-Fireworks 逻辑资源组与自适应物理仓库设计

## 1. 决策

HIT-Fireworks 采用“**培养方案身份独立、课程代码描述独立、课程组允许重叠、逻辑资源组互斥、物理仓库可承载多个逻辑资源组**”的模型。

1. `CurriculumRecord` 仍表示一门课程在一份培养方案中的一次出现；专业、学期、学分、学时等上下文绝不因资料共仓而丢失。
2. 每个完整课程代码都有一个独立 `CourseDescriptor`，记录规范名称、相关培养方案记录、逻辑资源组和物理仓库。
3. `CourseGroup` 由培养方案范围声明物化，表达可重叠的课程相关性；同一课程代码可以同时属于多个课程组。
4. `CourseGroup` 只用于检索、导航、仓库策略审阅和拆分血缘，不直接创建、合并或拆分资料仓库。
5. 每个课程代码恰好属于一个 `LogicalResourceGroup`；该逻辑分区由精确同名基础组、显式基础组和冻结课程族配置共同决定。
6. 规范化课程名称完全相同时自动形成基础资源分组。
7. 名称不同的基础共组只能由版本化配置 `config/resource-repository-groups.v1.json` 显式批准。
8. 两个或更多基础资源分组可以由版本化配置 `config/course-resource-families.v1.json` 物化为课程族；课程族只改变生效逻辑资源组，不直接指定物理仓库。
9. 正式 manifest 使用冻结的 `aggressive-policy` 课程族阶段；`approved-candidates` 阶段作为保守中间结果保留。运行期不执行代码尾字母、相似度或学科归属推断，只消费已审阅的成员列表。
10. 每个逻辑资源组恰好归属一个 `PhysicalRepository`，而一个物理仓库可以承载多个逻辑资源组。
11. 物理归属只由版本化配置 `config/physical-repository-policy.v1.json` 决定：独立重算各逻辑资源组关联培养方案记录的 canonical owner，再按冻结共享前沿物化；运行期不接受其他自动重映射。
12. 物理仓库保存成员逻辑资源组、首选来源仓库名和 `CourseGroup` 拆分血缘，后续拆分无需从仓库名或课程代码猜测来源。
13. 1,959 条无代码记录继续按来源培养方案进入 141 个 `requirement-set` 仓库；每条记录有独立 `record_id`。
14. `fireworks-attachments@15d3ceff51e1bcd6f8ba16e1b3fade20bf3cf99e` 的 3,857 个内容文件全部且仅迁移一次，沿用当前清洗结果，不做第二轮版权或敏感资料复审。
15. `CourseGroup` 派生的仓库合并与拆分建议全部标记为 `review-required`，不会自动修改 manifest、课程族配置或物理仓库策略。

机器可审计输入：

- 培养方案：`.workspaces/hoahrb-jwts/hit-data/plans/`；
- 课程组声明：`config/course-groups.v1.json`；
- 基础资源分组：`config/resource-repository-groups.v1.json`；
- 课程族成员：`config/course-resource-families.v1.json`；
- 基础组到生效逻辑资源组的全量映射：`config/course-resource-family-migration.v1.json`；
- 生效逻辑资源组到物理仓库的互斥策略：`config/physical-repository-policy.v1.json`；
- 课程族候选分析冻结报告：`data/course-resource-family-candidates.v1.json`；
- 资料感知仓库策略候选：`data/repository-policy-candidates.v1.json`；
- 冻结附件树：`HIT-Fireworks/fireworks-attachments@15d3ceff51e1bcd6f8ba16e1b3fade20bf3cf99e`；
- 正式生成结果：`data/repository-manifest.json`；
- 保守阶段结果：`data/repository-manifest.approved-candidates.v2.json`。

manifest 生成器为 `scripts/generate-repository-manifest.py`，资料感知候选生成器为 `scripts/generate-repository-policy-candidates.py`，独立校验器为 `scripts/validate-repository-manifest.py`。生成器、独立校验器和建仓器分别消费同一版本化物理仓库契约；校验器不调用生成器内部逻辑，而是从培养方案重新计算 canonical owner、物理归属和拆分血缘。

## 2. 冻结数据与仓库规模

### 2.1 培养方案

| 指标 | 数量 |
|---|---:|
| HIT 本部 2022 版培养方案 | 211 |
| 培养方案记录 | 10,468 |
| 有代码记录 | 8,509 |
| 无代码记录 | 1,959 |
| 不同课程代码 | 2,618 |
| 不同有代码课程名称 | 1,945 |

### 2.2 课程组

| 指标 | 数量 |
|---|---:|
| 课程组声明 | 5 |
| `CourseGroup` | 392 |
| 单培养方案组 / 专业组 / 学院组 | 211 / 134 / 20 |
| 专业选修组 / 学院选修组 | 19 / 8 |
| descriptor—课程组成员关系 | 19,540 |
| 同时属于多个课程组的课程代码 | 2,618 |
| 直接引起的资源分组或仓库变更 | 0 |

课程组是可重叠的语义层。392 个组覆盖全部 2,618 个课程代码，成员关系由源培养方案和 `config/course-groups.v1.json` 重算；它们不参与互斥仓库归属计算。

### 2.3 基础资源分组与生效逻辑资源组

| 指标 | 数量 |
|---|---:|
| `CourseDescriptor` | 2,618 |
| `BaseResourceGroup` | 1,583 |
| 基础组中含多个代码 | 475 |
| 显式审阅基础组 | 211 |
| 自动精确同名基础组 | 1,372 |
| `approved-candidates` 课程族 / 生效逻辑资源组 | 27 / 1,545 |
| `aggressive-policy` 课程族 / 生效逻辑资源组 | 141 / 1,303 |
| 正式结果中含多个代码的逻辑资源组 | 490 |

基础分组相对“一代码一逻辑组”减少 1,035 个分组。正式课程族阶段再吸收 280 个基础组，得到 1,303 个互斥逻辑资源组。这些数字描述逻辑维护归属，不等于物理 GitHub 仓库数量。

### 2.4 自适应物理仓库

正式 `aggressive-policy` 使用 1,303 个逻辑资源组物化 209 个策略内物理课程仓库，减量 1,094。另有 17 个尚未绑定教务课程名的稳定历史课程仓库，因此正式 manifest 共规划 403 个仓库：

| `repo_type` | 数量 | 用途 |
|---|---:|---|
| `course` | 226 | 209 个策略内物理课程仓库，加 17 个未绑定逻辑资源组的稳定历史资料仓库 |
| `requirement-set` | 141 | 逐培养方案维护无代码记录 |
| `collection` | 21 | 学院、实验和待识别集合导航 |
| `competition` | 5 | 竞赛专题 |
| `shared` | 4 | 跨课程共享资料 |
| `software` | 1 | 完整软件与教程 |
| `control` | 2 | 注册表与组织管理 |
| `template` | 3 | 课程资料、培养要求和集合模板 |
| **合计** | **403** | |

保守 `approved-candidates` 中间 manifest 为 1,545 个逻辑资源组、232 个策略内物理课程仓库、249 个全部 `course` 仓库和 426 个总仓库；相对逻辑资源组减量 1,313。它用于审计两阶段差异，不是正式建仓输入。

canonical owner 是每个逻辑资源组关联的全部培养方案记录在 `offering_college → school_name → major_name → course_category` 上的最长公共前缀。正式阶段深度分布为 `0:62 / 1:112 / 2:284 / 3:8 / 4:837`：深度小于 2 的组各自进入专用仓库；深度至少为 2 的组按前两级共享前沿物化。`physical_repository_id` 来自物化关系的稳定摘要，`repo_id` 只是 GitHub 名称，不承担逻辑身份。

旧 canary 构成冻结的前一控制面世代：

- `fireworks-course-registry@29491c2d8a19e80293e3b07a0399667a46929e39`；
- `fireworks-repos-management@1b1fc9309f1944e7e4da502b97f6ba4321e3f676`；
- `fireworks-course-template@73ac884969d1cf365e2995608ab6cde91bf56c9e`；
- `22AD11001@828b3b586aad439de72a93a2288201ec827990ba`。

v2 使用新的 `-v2` 注册表、管理仓库和三个模板；思想政治理论实践课资源仓库改用组内未占用代码 `22AD11001C`。旧四仓不得出现在 v2 manifest 或 `template_id` 中，不会被强推、覆盖、重建或复用。dry-run、canary、apply 和 verify 均以旧四仓的精确名称、公开未归档状态、template 标记、默认分支及冻结 commit/tree 为前置条件；任一漂移即停止。其余仓库尚未全量创建。

## 3. 领域模型

### 3.1 `CurriculumRecord`

培养方案中的一次课程或培养要求出现，主键为 `record_id`。保存：

- 来源培养方案、源文件和序号；
- 学院、专业、建议学期；
- 课程名、可空课程代码；
- 学分、总学时、分项学时、考核方式、课程性质和类别；
- `descriptor_id`、`resource_group_id`、`physical_repository_id` 与 `repo_id`；
- 来源路径、映射状态和身份状态。

### 3.2 `CourseDescriptor`

一个完整课程代码的独立描述。主键格式为 `course-code:<course_code>`，保存：

- `course_code` 与规范课程名；
- 该代码对应的全部 `record_id`；
- `resource_group_id`、`physical_repository_id` 与 `repo_id`；
- 对应培养方案来源路径。

资料共仓只改变 descriptor 的目标仓库，不合并 descriptor 本身。

### 3.3 `CourseGroup`

由声明式培养方案范围物化的相关课程集合。每组保存稳定 `course_group_id`、`group_type`、`scope_conditions`、`relation_key`、成员语义、证据、课程代码和记录快照。当前支持单培养方案、同学院同名专业、培养方案归属学院，以及专业/学院选修范围。

课程组允许重叠，且不保存 `resource_group_id` 或 `repo_id`。成员关系不参与资料仓库分配；独立校验器从培养方案和配置完整重算 392 个组与 19,540 条成员关系。

### 3.4 `BaseResourceGroup`

课程资料共仓的最小审阅结果。每组保存基础 `group_id`、代表 `repo_id`、课程名、课程代码、形成规则和可选历史资料单元。形成方式只有 `exact-normalized-course-name` 与 `explicit-reviewed`；基础组集合及其摘要是课程族配置的冻结输入。

### 3.5 `CourseFamily`

两个或更多基础资源分组的显式逻辑聚合。每个课程族保存稳定 `family_id`、首选 `repo_id`、完整成员 `group_id` 列表、派生课程名/代码/历史单元、审批证据和来源仓库列表。`family_id` 由稳定首选仓库生成；成员变化不会悄然改名。课程族不表示课程教学等价，不删除任何 descriptor 或 record，也不直接决定物理仓库。

### 3.6 `LogicalResourceGroup`

正式 manifest 中生效的互斥逻辑资料分组。未被吸收的基础组原样保留；课程族以 `materialized-course-family` 规则替换其成员基础组。每个逻辑资源组保存 `preferred_repo_id`、canonical owner、唯一 `physical_repository_id` 和拆分血缘；多个逻辑资源组可以指向同一物理仓库。

### 3.7 `PhysicalRepository`

实际承载资料的 GitHub 仓库。策略内 `course` 仓库保存稳定 `physical_repository_id`、`member_resource_group_ids`、`preferred_source_repo_ids`、canonical owner 范围、课程代码并集和 `split_lineage_course_group_ids`。它不得携带单值 `resource_group_id`；仓库名 `repo_id` 不替代物理或逻辑身份。

### 3.8 `RequirementSet`

一份培养方案中全部无代码记录的维护仓库。同仓依据是来源培养方案，不是课程等价性。未来获得代码时增加映射，不删除原记录。

### 3.9 `LegacyUnit`

冻结附件树中的稳定迁移单元。它保存源 commit、原路径、生效逻辑资源组和目标物理仓库。基础组被课程族吸收时，历史资料单元必须先随全量迁移映射指向课程族逻辑组，再沿物理仓库策略进入唯一目标；文件覆盖仍保持全部且仅一次。

## 4. 分组规则

### 4.1 课程组规则

`config/course-groups.v1.json` 声明五类范围：单份培养方案、同学院同名专业、培养方案归属学院、专业选修课程和学院选修课程。生成器只按 `source_plan`、`school_name`、`major_name` 与 `course_nature` 的精确值筛选并物化成员，不做名称相似度或课程代码推断。

每个课程代码可以属于多个课程组。课程组及其成员关系不得含 `resource_group_id` 或 `repo_id`；新增、删除或修改课程组不改变 descriptor、资源分组、仓库或历史附件的归属。

### 4.2 基础自动规则

规范化课程名称完全相同的所有代码自动进入同一基础资源组。例如同名的“毕业论文（设计）”代码共享资料仓库；各专业、学分和学时差异仍保留在 `CurriculumRecord` 与 `CourseDescriptor` 中。

### 4.3 基础显式规则

不同名称形成同一基础组必须写入 `config/resource-repository-groups.v1.json`。生成器强制校验 `group_id` 和 `repo_id` 唯一、代表代码属于组内、课程名存在、预期代码无漂移、课程名和历史单元不被重复分配，以及课程代码—名称摘要未变化。

### 4.4 课程族逻辑分组策略

`config/course-resource-families.v1.json` 物化两个已审阅阶段：

1. `approved-candidates`：27 个课程族，将 1,583 个基础组变为 1,545 个生效逻辑资源组；
2. `aggressive-policy`：141 个课程族，将 1,583 个基础组变为 1,303 个生效逻辑资源组，作为正式默认阶段。

每个课程族至少包含两个互不重叠的基础组，首选 `repo_id` 必须是族内真实课程代码且不得命中冻结旧控制面。`config/course-resource-family-migration.v1.json` 将每个基础 `group_id` 和首选 `repo_id` 恰好映射一次到最终逻辑资源组；未合并项为 `retain`，被吸收项为 `merge`。源基础组数量和规范化摘要任一漂移都会停止生成。

课程族配置明确声明 `role = exclusive-resource-repository-policy` 和 `course_groups_do_not_imply_repository_merge = true`。这里的历史字段名只表示课程族阶段的互斥逻辑归属；物理仓库由下一层独立策略决定。课程族候选报告中的名称归一化、相似度阈值和学科约束只记录离线分析依据；生成器、独立校验器和建仓器均不在运行期重算课程族。

### 4.5 物理仓库策略

`config/physical-repository-policy.v1.json` 是逻辑资源组到物理仓库的唯一互斥策略。它以所有关联培养方案记录重算 canonical owner，字段顺序固定为 `offering_college`、`school_name`、`major_name`、`course_category`。公共前缀深度小于 2 时，每个逻辑资源组保留专用物理仓库；深度至少为 2 时，按前两级共享前沿进入同一物理仓库。

策略按课程族阶段冻结生效逻辑资源组摘要、数量、物理仓库数量、总仓库数量和 owner 深度分布。生成器与独立校验器分别计算相同结果；建仓器再次校验每个逻辑资源组恰好出现一次、物理仓库课程代码并集、首选来源集合、descriptor/record/legacy 绑定和拆分血缘。`automatic_remapping` 恒为 `false`。

每个逻辑资源组把相关 `CourseGroup` 记录为 `split_lineage.course_group_ids`；物理仓库保存成员血缘的并集。未来拆分顺序固定为 `major_name → course_category → resource_group`，但任何拆分都必须先修改版本化策略并重新生成，不允许运行期自行拆分。

### 4.6 资料感知仓库策略候选

`data/repository-policy-candidates.v1.json` 是只读审阅输入，不是 manifest 补丁：

- 合并候选只包含完整落在同一课程组内、且当前附件文件数均为 0 的生效逻辑资源组；
- 每个合并候选最多包含 10 个互斥逻辑资源组，超出上限的课程组只记录为超大范围，不自动拆批；
- 因课程组重叠而共享逻辑资源组的候选显式记录冲突，不能同时批准；
- 拆分候选要求同一现有课程族中至少两个基础资源分组各自已有历史附件；
- 所有候选状态均为 `review-required`，`automatic_application` 恒为 `false`。

当前报告包含 151 个去重合并候选、150 个超大范围和 4 个拆分候选；120 个合并候选存在显式冲突。任何候选获批后仍须单独更新课程族配置、全量迁移映射和物理仓库策略，并重新生成与校验 manifest。

### 4.7 明确禁止

- 把 `CourseGroup` 当作 `LogicalResourceGroup`、课程族或物理仓库合并指令；
- 把一个逻辑资源组等同于一个物理仓库，或从 `repo_id` 反推稳定身份；
- 运行期剥离 `A/B/C/D/E/F`、罗马数字、上下册或序列号后自动合并；
- 仅凭代码前缀、尾字母、学院、学分、学时或模糊字符串相似度修改逻辑组或物理仓库成员关系；
- 因资料共仓删除或合并课程代码描述、培养方案记录；
- 课程族或物理策略只更新一层，却遗漏 descriptor、record 或历史资料单元的目标；
- 将保守阶段、激进阶段或不同逻辑资源组摘要的物理策略混用。

## 5. 仓库内部结构

### 5.1 `course` 资料仓库

```text
<repo_id>/
├── .github/workflows/sync.yml
├── README.md
├── repository.toml
├── notes/
├── slides/
├── assignments/
├── exams/
├── labs/
├── textbooks/
├── tutorials/
├── projects/
├── software/
├── variants/
│   └── <course_code>/
└── legacy/
```

共享资料放在顶层语义目录；只有确属某一代码的差异资料放入 `variants/<course_code>/`。一个仓库可服务多个逻辑资源组和课程代码，成员关系与培养方案绑定以中央 manifest 为权威源。

### 5.2 `requirement-set`

```text
REQ-HIT-.../
├── README.md
├── repository.toml
├── requirements.json
├── records/<record_id>/README.md
├── records/<record_id>/notes/
├── records/<record_id>/resources/
├── mappings/
└── legacy/
```

### 5.3 `collection`

```text
<collection>/
├── README.md
├── repository.toml
├── shared/
├── members/
└── legacy/
```

集合只提供导航与共享资料，不复制成员仓库，也不改变课程代码描述。

## 6. Manifest v2 契约

`data/repository-manifest.json` 包含：

```text
sources.course_groups
sources.course_families
sources.physical_repositories
repositories[]
resource_groups[]
course_groups[]
course_group_memberships[]
course_descriptors[]
curriculum_records[]
legacy_roots[]
legacy_units[]
```

核心不变量：

- `sources.course_groups` 固化配置、培养方案摘要、声明数、组数、成员关系数和“不触发仓库合并”策略；
- 392 个课程组与 19,540 条成员关系可由培养方案和配置独立重算；
- 课程组允许重叠，全部 2,618 个课程代码都属于多个课程组；课程组及成员关系不含仓库或逻辑资源组归属；
- `sources.course_families` 固化配置、迁移文件、阶段、基础摘要及 1,583 → 1,303 的逻辑组计数；
- `sources.physical_repositories` 固化物理策略、同阶段逻辑组摘要、canonical owner 算法、层级字段、共享前沿和 1,303 → 209 的物理仓库计数；
- 2,618 个源课程代码各有且仅有一个 descriptor；
- 每个 descriptor 和有代码培养方案记录指向且仅指向一个生效逻辑资源组和一个物理仓库；
- 每个无代码记录不绑定 descriptor、逻辑资源组或策略内物理课程仓库；
- 每个基础资源组在课程族迁移映射中恰好出现一次；课程族成员互不重叠，物化字段与成员基础组的并集完全一致；
- 每个生效逻辑资源组在 `member_resource_group_ids` 中恰好出现一次；策略内物理课程仓库不得携带单值 `resource_group_id`；
- 每个物理课程仓库的课程代码、首选来源仓库和拆分血缘，分别等于其成员逻辑资源组对应字段的并集；
- canonical owner、物理仓库 ID、`repo_id`、owner 深度分布和阶段计数可由源培养方案与策略独立重算；
- descriptor、培养方案记录和历史资料单元的 `resource_group_id`、`physical_repository_id` 与 `repo_id` 三者一致；
- 3,857 个历史文件全部且仅分配一次，被吸收基础组的历史单元先指向课程族逻辑组，再指向该组唯一物理仓库；
- 所有目标 `repo_id` 存在、大小写无冲突且不引用冻结旧控制面。

## 7. 历史资料迁移

目录语义明确时转换：

| 原目录语义 | 目标目录 |
|---|---|
| 课程笔记 | `notes/` |
| 帮辅讲座、考前讲座 | `tutorials/` |
| 课件 | `slides/` |
| 习题、作业 | `assignments/` |
| 模拟题、历年题 | `exams/` |
| 实验资料 | `labs/` |
| 电子教材 | `textbooks/` |
| 程序、工程树 | `software/` 或独立软件仓库 |
| 无法无损判断 | `legacy/<原相对路径>` |

迁移器不根据扩展名猜用途。配置绑定的历史单元直接进入指定资源仓库；课程名精确匹配时进入对应资源仓库；无法确认时保留为独立 `LEGACY-*` 或集合仓库。

## 8. 执行流程

### 8.1 生成和校验

```bash
# 生成正式 aggressive-policy manifest、候选报告并独立校验
python scripts/generate-repository-manifest.py
python scripts/generate-repository-policy-candidates.py
python scripts/validate-repository-manifest.py

# 生成并校验保守阶段，用于审计阶段差异
python scripts/generate-repository-manifest.py --course-family-stage approved-candidates --output data/repository-manifest.approved-candidates.v2.json
python scripts/validate-repository-manifest.py --manifest data/repository-manifest.approved-candidates.v2.json
```

manifest 进入一次建仓批次后冻结。后续培养方案、课程组声明、课程族成员或物理仓库策略变化必须重新生成；仓库归属变化还必须生成 split/merge diff、更新对应版本化输入，并重新通过独立校验。资料感知候选报告不会自行改变任何权威输入。

### 8.2 建仓安全闸门

建仓器必须：

- `--apply` 明确指定 `--repo-id`、`--max-repositories` 或 `--all-repositories`；
- `--batch-size` 只控制 API 节奏，不冒充总量上限；
- 达到总量上限时原子保存 execution report，并以成功的 partial 状态退出；
- 每完成一个仓库立即保存记录；
- 重跑时从 execution report 与远端实际状态恢复；
- v2 manifest 必须完整包含五个 `-v2` 控制/模板仓库，禁止引用冻结旧世代四仓；
- dry-run 固化旧世代四仓状态与 commit/tree 快照，canary、apply 和 verify 均重新核验；
- 已存在 v2 control/template 仓库只校验 execution report 或本地预期 tree，不强推、不覆盖、不自动修复；
- 模板 tree 不一致时停止；
- GitHub 限流按响应头等待，不通过高并发绕过。

执行顺序：

1. 用 1 个未创建仓库验证 partial-success；
2. 重跑同一目标确认幂等复用；
3. 用 10 个仓库验证跨进程续跑；
4. 冻结新 dry-run、execution 和 canary 报告；
5. 明确使用 `--all-repositories` 后才允许全量创建。

### 8.3 建仓与内容迁移分离

先创建并初始化正式 manifest 中的 403 个仓库，再迁移历史内容。迁移时逐 `LegacyUnit`：

1. 计算源文件摘要；
2. 按 manifest 写入目标路径；
3. 校验目标数量、字节和 SHA；
4. 记录源 commit、原路径、目标仓库和目标路径；
5. 所有文件零遗漏、零重复后再切换展示链接；
6. 旧附件仓库不自动删除，归档作为独立决策。

## 9. 接受标准

- [x] 培养方案身份、逻辑资源归属与物理仓库解耦；
- [x] 一个物理仓库可绑定多个逻辑资源组和多个课程代码；
- [x] 每个课程代码有独立 descriptor；
- [x] 五类声明物化为 392 个可重叠课程组和 19,540 条成员关系；
- [x] 课程组不含仓库归属，新增课程组不改变 2,618 个 descriptor 的逻辑资源归属；
- [x] 精确同名课程自动形成同一基础资源组；
- [x] 不同名称的基础共组全部固化为显式配置；
- [x] 基础分组将 2,618 个课程代码归入 1,583 个基础资源组；
- [x] 激进课程族阶段将 1,583 个基础组物化为 1,303 个生效逻辑资源组；
- [x] 激进物理策略将 1,303 个逻辑资源组互斥归入 209 个策略内物理课程仓库，正式 manifest 总量为 403；
- [x] 保守阶段将 1,545 个逻辑资源组互斥归入 232 个策略内物理课程仓库，manifest 总量为 426；
- [x] 每个物理仓库保留成员逻辑资源组、canonical owner 和课程组拆分血缘；
- [x] manifest v2 覆盖 10,468 条培养方案记录、2,618 个代码 descriptor 和 3,857 个历史文件；
- [x] 独立校验器从源数据重算课程组、课程族、逻辑迁移、canonical owner、物理归属与源覆盖，确认零遗漏且零重复；
- [x] 建仓器消费并校验物理仓库数量、互斥成员覆盖、课程代码并集、首选来源和拆分血缘；
- [x] 资料感知仓库策略报告只生成 `review-required` 候选，附件门槛、单候选上限和重叠冲突均可审计；
- [ ] 建仓器总量上限与 partial-success 续跑验证完成；
- [ ] 403 个远程仓库全部创建并初始化；
- [ ] 远程仓库列表与冻结 manifest 一一核验；
- [ ] 3,857 个历史文件完成摘要一致的迁移。
