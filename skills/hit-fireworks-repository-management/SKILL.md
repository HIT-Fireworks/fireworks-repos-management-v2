---
name: hit-fireworks-repository-management
description: 管理 HIT-Fireworks 仓库状态：检查 v4 健康、搜索仓库与课程映射、浏览路由、审阅 split/merge 冻结计划、执行已明确批准的 plan identity、verify 最终状态或按 journal 恢复。必须通过本 Skill 的 JSON 核心；不得直接改 topology/routes 或调用 GitHub mutation。
---

# HIT-Fireworks 仓库管理 Skill

这是管理产品的 Agent 适配器。唯一外部接口是产品仓中的 JSON 核心：

```sh
python scripts/fireworks_manager_core.py --workspace <WORKSPACE> invoke < request.json
```

请求从 stdin 读取 `CommandEnvelope`，响应从 stdout 返回 `ResponseEnvelope`。不要直接读取或改写 manifest、topology、routes，不要临时拼 GitHub API。人类 TUI 与本 Skill 共享同一核心，不复制领域规则。

## 工作区与核心定位

默认权威数据：

```text
data/repository-manifest.no-collection.v4.json
data/repository-management-operations/       # 可不存在
config/repository-topology.v4.json
config/repository-file-routes.v4.json
```

`context.workspace` 只指定数据工作区。Rust TUI 的核心脚本通过 `FIREWORKS_MANAGER_CORE`、开发树或可执行文件祖先路径定位；外部 workspace 不要求包含 `scripts/`。

## 四页工作台对应查询

- **Repositories**：`query.search`、`query.repository`。搜索 repo_id、语义、课程代码和原始课程名；详情含资源组、无归属路径、文件库存、HEAD 与 physical repository。
- **Routes**：`query.routes`。返回 `file_routes`、`course_code_routes` 及总数；`limit=0` 明确表示全量，省略时默认 200；路由查询只读。
- **Plans**：`query.plan`。无 `data/repository-management-operations` 目录时返回 `{"plans":[]}`；读取计划必须校验其完整 identity。
- **Journals**：`query.journals`。返回 `status`、`recovery_state`、plan identity、路径和错误；状态分类为 `completed`、`resumable`、`drifted`、`invalid`。

## 必须遵循的流程

1. **Inspect**：先 `query.inspect`。必须 `ok=true`、`mode=read_only`、`result.health=healthy`；保存三项 workspace identity。
2. **Locate**：用 `query.search`、`query.repository`、`query.routes` 确认每个 repo_id、课程代码、资源组和文件边界；不能从仓库名猜测。
3. **Plan**：执行 `plan.split` 或 `plan.merge`。计划会持久化并返回 path、operation_id、风险、完整 `plan_identity_sha256` 和 APPLY challenge。
4. **Review**：向用户逐项报告源/目标仓、资源组、文件移动、冲突 relocation、远端创建、Registry、actor、源 commit/tree、目标存在性/HEAD 和 workspace baseline。完整保留机器字段。
5. **Approve gate**：未获得用户对当前完整 `plan_identity_sha256` 的明确批准，停止；不能用简称、旧计划或 `--yes`。
6. **Apply**：按 [Mutation](references/mutation.md) 回传核心给出的精确 `APPLY <operation-id>`。完成条件是 journal `completed`，不是进程退出或单个 push 成功。
7. **Verify**：对 completed journal 执行 `execute.verify`，确认最终 topology/routes、远端 HEAD 和 identity 闭合。
8. **Recover**：先 `query.journals`；仅 `recovery_state=resumable` 按 [Recovery](references/recovery.md) 重新查询远端并输入精确 `RESUME <operation-id>`。`drifted` 或 `invalid` 必须重新 inspect/plan。

## Plan 参数

### Split

```json
{
  "schema_version": 1,
  "request_id": "plan-split",
  "command": {
    "family": "plan",
    "kind": "split",
    "arguments": {
      "source_repo_id": "COURSE-A",
      "targets": [
        {
          "repo_id": "COURSE-A1",
          "display_name": "课程一",
          "resource_group_ids": ["group-a"],
          "paths": ["README.md"]
        },
        {
          "repo_id": "COURSE-A2",
          "display_name": "课程二",
          "resource_group_ids": ["group-b"],
          "paths": []
        }
      ]
    }
  },
  "context": {"workspace": ".", "organization": "HIT-Fireworks", "actor": "agent"},
  "confirmation": null
}
```

Split 要求源仓库完整库存冻结；全部资源组必须恰好分配一次；无资源组文件必须显式列入一个目标；目标不能重复或产生空目标。

### Merge

```json
{
  "command": {
    "family": "plan",
    "kind": "merge",
    "arguments": {
      "source_repo_ids": ["COURSE-A", "COURSE-B"],
      "target_repo_id": "COURSE-A",
      "display_name": "课程 A/B"
    }
  }
}
```

Merge 要求所有源仓库完整库存冻结；同路径不会静默覆盖，核心会生成 `merged-from/<source_repo_id>/...` relocation 并写入风险摘要。

计划冻结并绑定：

- plan 内容 identity（排除 `created_at` 与 identity 自身后重算）；
- organization、请求 actor 和 workspace manifest/topology/routes identity；
- Registry manifest/baseline；
- 源仓库 commit/tree；
- 目标仓库存在性与 HEAD、remote URL；
- confirmation phrase。

执行过程不会回写冻结计划；远端目标 HEAD 解析存放在 journal 的 `resolved_after_routes`。

## 只读示例

```json
{"schema_version":1,"request_id":"inspect","command":{"family":"query","kind":"inspect","arguments":{}},"context":{"workspace":".","organization":"HIT-Fireworks","actor":"agent"},"confirmation":null}
```

```json
{"schema_version":1,"request_id":"routes","command":{"family":"query","kind":"routes","arguments":{"limit":0}},"context":{"workspace":".","organization":"HIT-Fireworks","actor":"agent"},"confirmation":null}
```

```json
{"schema_version":1,"request_id":"journals","command":{"family":"query","kind":"journals","arguments":{}},"context":{"workspace":".","organization":"HIT-Fireworks","actor":"agent"},"confirmation":null}
```

## 错误处理

以下任一状态都不能改字段后重试：`organization_mismatch`、`plan_identity_mismatch`、`workspace_drifted`、`confirmation_required`、`remote_drifted`、`journal_invalid`、`state_invalid`。先保留完整响应和 evidence，再重新 inspect/plan 或按恢复流程处理。

`query` 永远是 `read_only`，不得创建 operations 目录。TUI 仅通过核心调用；它不能直接改写状态文件或调用 GitHub。

详细字段和门禁见：

- [Schema](references/schema.md)
- [Mutation](references/mutation.md)
- [Recovery](references/recovery.md)
