# JSON Schema 契约

核心 schema 版本：`1`。未知主版本必须停止，不能猜测兼容。

## CommandEnvelope

```json
{
  "schema_version": 1,
  "request_id": "stable-caller-id",
  "command": {
    "family": "query | plan | execute",
    "kind": "...",
    "arguments": {}
  },
  "context": {
    "workspace": ".",
    "organization": "HIT-Fireworks",
    "actor": "human | agent | tui"
  },
  "confirmation": null
}
```

稳定请求字段：`schema_version`、`request_id`、`command.family`、`command.kind`、`command.arguments`、`context.workspace`、`context.organization`、`context.actor`、`confirmation`。

## ResponseEnvelope

```json
{
  "schema_version": 1,
  "request_id": "stable-caller-id",
  "ok": true,
  "mode": "read_only | planned | applying | completed | drifted | failed",
  "result": {},
  "warnings": [],
  "errors": [],
  "evidence": [],
  "next_actions": []
}
```

`errors[]` 稳定结构为 `{code,message,retryable}`。调用者只能依赖 Envelope、本文声明的 command result 字段、plan identity、journal recovery state 和错误码；展示字段可新增。

## Query

### `query.inspect`

结果：

- `organization`
- `health`: `healthy | invalid`
- `health_message`
- `identity`: `{manifest_sha256,topology_sha256,routes_sha256}`
- `repository_count`、`repository_types`
- `course_route_count`、`file_route_count`
- `inventory_complete_repository_count`
- `virtual_collection_count`、`special_topic_route_count`

### `query.validate`

状态有效时返回 `{valid:true,identity}`；无效时 `state_invalid`。

### `query.search`

参数：

- `term`：匹配 repo_id、display_name、description、课程代码、原始课程名；
- `repo_type`：可选精确过滤；
- `has_content`：可选布尔过滤；
- `limit`：1–500，省略为 50。

结果：`{total,repositories[]}`。仓库项含语义、类型、description、课程代码、原始课程名、资源组、无归属路径、文件数/字节、HEAD、库存完整性。

### `query.repository`

参数：`repo_id`。结果在搜索项基础上增加 `file_routes`、`course_routes` 和 topology 仓库记录。

### `query.routes`

参数：

- `route_kind`、`repo_id`：可选过滤；
- `limit`：省略为 200；`0` 明确表示返回全部；其他值限制为 1–10,000。

结果：`{file_total,course_code_total,file_routes[],course_code_routes[]}`。total 是过滤后全量计数，与返回切片长度分开。

### `query.plan`

- 无 `operation_id`：`{plans:[...]}`；operations 目录不存在时返回空数组且不创建目录；
- 有 `operation_id`：`{path,plan,plan_identity_sha256,state}`。

计划摘要包含 `valid`、`error`、`operation_id`、`kind`、`created_at`、完整 identity、confirmation phrase 和 workspace `state`（`before | topology-applied | after | drifted`）。

### `query.journals`

结果：`{journals:[...]}`。每项包含 path、operation_id、kind、status、`recovery_state`、plan identity、RESUME challenge、error、updated_at。

`recovery_state`：

- `completed`：journal completed 且 workspace 为最终 after；只能 verify；
- `resumable`：journal planned/applying/failed 且 workspace 处于允许阶段；
- `drifted`：workspace 不属于计划 before/topology-applied/after；
- `invalid`：journal、内嵌计划、Git/provisioning 状态或 identity 非法。

## Plan

### `plan.split`

```json
{
  "source_repo_id": "COURSE-A",
  "targets": [
    {
      "repo_id": "COURSE-A1",
      "display_name": "课程一",
      "resource_group_ids": ["group-a"],
      "paths": ["README.md"]
    }
  ]
}
```

至少两个目标。资源组必须形成完整互斥分区；无资源组文件必须显式裁决；目标不得为空。可选 `remote_url_template` 仅用于受控本地/bare-remote 测试，生产默认 GitHub URL。

### `plan.merge`

参数：`source_repo_ids`（至少两个）、`target_repo_id`、可选 `display_name`、可选受控测试 `remote_url_template`。目标若已存在必须位于源集合。路径冲突以 relocation 明确记录，不覆盖。

两类计划结果：

```json
{
  "path": "...plan.json",
  "plan": {
    "operation_id": "operation-...",
    "before": {},
    "after": {},
    "details": {},
    "core": {
      "organization": "HIT-Fireworks",
      "workspace_identity": {},
      "request_actor": "agent",
      "confirmation_phrase": "APPLY operation-...",
      "remote_baseline": {},
      "plan_identity_sha256": "..."
    }
  },
  "risk": {
    "remote_mutation": true,
    "file_move_count": 0,
    "source_repo_ids": [],
    "target_repo_ids": [],
    "new_target_repo_ids": []
  }
}
```

`remote_baseline` 冻结 Registry、actor、源 commit/tree、目标存在性/HEAD 和 remote URL。计划 after 不会在执行时修改；最终解析 routes 位于 journal `resolved_after_routes`。

## Execute

### `execute.apply`

参数：`plan`（核心返回 path）、`plan_identity_sha256`；`confirmation` 必须精确等于计划 challenge。仅接受 workspace `before`，无既有 journal，远端 baseline 未漂移。

### `execute.resume`

参数：`journal`、journal 的完整 `plan_identity_sha256`；`confirmation` 必须精确为 `RESUME <operation-id>`。只接受未 completed、有效且可续跑的 journal。

### `execute.verify`

参数：`journal`、完整 plan identity；无需 confirmation。只接受 completed journal 和 workspace `after`，重新验证 topology/routes、远端 baseline、目标 commit 和最终 HEAD。

## 错误码

- 输入/来源：`schema_incompatible`、`invalid_request`、`source_missing`、`source_invalid`、`repository_not_found`
- 状态/契约：`state_invalid`、`contract_violation`、`organization_mismatch`、`workspace_drifted`、`remote_drifted`
- 身份/确认：`plan_identity_mismatch`、`github_identity_missing`、`github_identity_mismatch`、`confirmation_required`
- journal：`journal_invalid`、`journal_exists`、`journal_not_resumable`、`verification_incomplete`
- 远端：`remote_unavailable`（可重试性由 `retryable` 给出）、`remote_invalid`
- 分发：`unsupported_family`、`unsupported_query`、`unsupported_plan`、`unsupported_execute`
- 未分类：`internal_error`

任何 identity、organization、workspace 或 remote 漂移错误都不得改字段后重试；重新 inspect/plan 或人工审计。
