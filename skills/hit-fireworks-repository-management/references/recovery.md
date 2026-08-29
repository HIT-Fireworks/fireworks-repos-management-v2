# Recovery 恢复契约

恢复必须先观察，再行动。任何进程异常、网络错误或客户端超时都不等于远端未执行。

## 1. 查询 journal

```json
{
  "schema_version": 1,
  "request_id": "journals",
  "command": {"family":"query","kind":"journals","arguments":{}},
  "context": {"workspace":".","organization":"HIT-Fireworks","actor":"agent"},
  "confirmation": null
}
```

operations 目录不存在时返回空数组，不创建目录。每项返回 `status`、`recovery_state`、`operation_id`、完整 plan identity、路径和错误。

- `completed`：停止 resume；执行 `execute.verify`；
- `resumable`：允许按本文继续；
- `drifted`：停止，重新 inspect 和 plan；
- `invalid`：停止，保留证据并人工审计；
- `applying`/`failed` 原始 status 不自动等于可恢复，必须由核心计算 `recovery_state`。

## 2. 恢复前检查

核心必须验证：

1. journal 内嵌 plan 存在、operation_id/kind 一致；
2. plan identity 从完整计划内容重算一致；
3. organization、request actor、Registry baseline 一致；
4. workspace 处于 before、topology-applied 或已解析 after 的合法阶段；
5. journal `resolved_after_routes` 及其 hash 一致，且不修改冻结计划；
6. Git source/target 集合、remote URL、expected HEAD、目标 status/commit 与冻结 baseline 一致；
7. 当前远端重新读取后，已完成目标 commit 与远端 HEAD 一致；源仓库未漂移；
8. 目标仓库缺失只可按已确认 provisioning 记录幂等创建，已有非空仓库不得覆盖。

任一检查失败返回 `journal_invalid`、`workspace_drifted` 或 `remote_drifted`，不得改 journal 后继续。

## 3. Resume 请求

只有核心返回 `recovery_state=resumable` 才发送：

```json
{
  "schema_version": 1,
  "request_id": "resume",
  "command": {
    "family": "execute",
    "kind": "resume",
    "arguments": {
      "journal": "<core-returned-journal-path>",
      "plan_identity_sha256": "<journal-plan-identity>"
    }
  },
  "context": {
    "workspace": ".",
    "organization": "HIT-Fireworks",
    "actor": "agent"
  },
  "confirmation": "RESUME <operation-id>"
}
```

challenge 必须逐字匹配，包括大小写、空格和 operation id。恢复按 journal 已完成阶段继续：

- Git 阶段：重查远端；已完成目标只接受相同 commit；未完成目标重新检查 expected HEAD 后构造和 push；
- topology 阶段：只写冻结 after topology；
- routes 阶段：写入独立的 `resolved_after_routes`，不修改 plan；
- 最后重新 validate 和远端 HEAD verify，再标记 `completed`。

## 4. 网络与漂移

`remote_unavailable` 且 `retryable=true` 可以等待后再次 resume，但仍须使用同一 journal 和 plan identity。以下情况不可自动恢复：

- 源或目标 HEAD/tree 变化；
- 目标从空变为非空或被删除；
- Registry baseline 变化；
- GitHub actor 变化；
- topology/routes/manifest identity 变化；
- journal、plan、resolved routes 或 Git 状态被篡改。

重新生成计划前保留旧 journal 作为审计证据，不覆盖、删除或手工修补它。

## 5. 完成判定

恢复响应 `ok=true` 仍需检查 journal `status=completed`；随后执行 `execute.verify`，确认最终 workspace identity、routes HEAD、远端提交和无错误。未完成、failed、drifted、invalid 或 resumable 都不是交付完成。
