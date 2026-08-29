# Mutation 变更门禁

远端 mutation 只允许通过核心 `execute.apply` 或 `execute.resume`。TUI、Skill 和普通 CLI 不直接调用 GitHub API，不直接改写 manifest、topology、routes。

## Apply 流程

1. 先完成 `query.inspect`、目标定位和 `plan.split`/`plan.merge`。
2. 保存核心返回的完整：
   - `plan.path`；
   - `operation_id`；
   - `core.plan_identity_sha256`；
   - `core.confirmation_phrase`；
   - `core.remote_baseline`；
   - workspace identity。
3. 向用户说明源/目标仓库、资源组、文件移动、冲突 relocation、目标建仓、Registry、GitHub actor、源 commit/tree、目标存在性/HEAD 和风险。
4. 用户明确批准当前完整 identity 后，原样发送：

```json
{
  "schema_version": 1,
  "request_id": "apply",
  "command": {
    "family": "execute",
    "kind": "apply",
    "arguments": {
      "plan": "<core-returned-plan-path>",
      "plan_identity_sha256": "<full-plan-identity>"
    }
  },
  "context": {
    "workspace": ".",
    "organization": "HIT-Fireworks",
    "actor": "agent"
  },
  "confirmation": "APPLY <operation-id>"
}
```

核心在写 journal 和远端动作前验证：计划内容 identity、organization、workspace before identity、actor、Registry baseline、源 commit/tree、目标存在性/HEAD、remote URL 和精确 challenge。缺失目标仓库只在已确认操作中按 journal 幂等创建；已有非空目标不得覆盖。

完成条件：`ok=true`、journal `status=completed`、最终 topology/routes 有效；随后必须 `execute.verify`。进程退出、单个 push 成功、topology 已写入或 journal 创建都不是完成条件。

## Verify 流程

```json
{
  "command": {
    "family": "execute",
    "kind": "verify",
    "arguments": {
      "journal": "<core-returned-journal-path>",
      "plan_identity_sha256": "<full-plan-identity>"
    }
  },
  "confirmation": null
}
```

verify 只接受有效、completed 的 journal；必须处于最终 after 状态。核心重新校验：

- journal 内嵌计划未被替换；
- plan identity 与完整计划内容一致；
- workspace topology/routes 最终 hash 与 resolved after 一致；
- 所有目标 commit 可由冻结计划重建；
- 目标远端 `main` 与最终 routes HEAD 一致；
- Registry 和 actor baseline 未漂移。

## Resume 流程

resume 不是重新 apply。先 `query.journals`，只允许 `recovery_state=resumable` 的 journal。调用：

```json
{
  "schema_version": 1,
  "request_id": "resume",
  "command": {
    "family": "execute",
    "kind": "resume",
    "arguments": {
      "journal": "<core-returned-journal-path>",
      "plan_identity_sha256": "<full-plan-identity>"
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

恢复前核心重新读取远端事实并验证 journal 中固定的 remote URL、source/target 集合、expected HEAD、目标状态和 commit；远端请求失败可在 `retryable=true` 时稍后重试。失败后只按已完成阶段继续，禁止盲重试已完成 push。

## 立即停止条件

以下任一错误都不可通过修改请求字段绕过：

- `schema_incompatible`
- `state_invalid`
- `organization_mismatch`
- `plan_identity_mismatch`
- `workspace_drifted`
- `remote_drifted`
- `github_identity_missing`
- `github_identity_mismatch`
- `journal_invalid`
- `journal_exists`
- `confirmation_required`
- 目标存在性、HEAD、tree 或 Registry baseline 漂移

保留完整响应、evidence、plan 和 journal 路径；重新 inspect/plan 或人工审计。不得使用 `--yes`、简称 identity、旧 challenge 或手工修改 journal。
