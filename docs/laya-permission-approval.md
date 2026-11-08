# LayaAir permission approval correction

The pinned runtime previously updated only `permissions` when switching a live
Codex conversation to full access. Approval policy is a separate field. SetMode
now sends `approvalPolicy: never` with `:danger-full-access`, and restores
`on-request` for the built-in workspace/read-only tiers. Custom profiles keep
their existing policy. This remains a next-turn change; an existing approval
must still be answered explicitly.

Verified source: installed Codex CLI 0.161.0's experimental generated
`docs/protocol-schemas/codex-0.161.0/ThreadSettingsUpdateParams.json`.
An ephemeral native session with zero inference turns confirmed that changing
only the profile leaves `approvalPolicy: on-request`; sending both fields changes
it to `never`. Switching back with both fields restores `on-request`. Sanitized
observations are recorded in `permission-policy.live.json` beside the schema.

Always Allow previously always sent `acceptForSession`. The native schema scopes
that decision to its session approval cache. When a command offers a proposed
execpolicy amendment, Always Allow now sends the exact native proposed prefix
as `acceptWithExecpolicyAmendment`. An explicit availableDecisions list must
include that exact decision. Missing, malformed or unoffered proposals retain
the existing session decision; no prefix is inferred from shell text. Allow
Once, Reject, file approvals, MCP forms, authorization and questions retain
their separate response paths.

Verified sources: generated `CommandExecutionRequestApprovalParams.json`,
`CommandExecutionRequestApprovalResponse.json` and
`FileChangeRequestApprovalResponse.json` in the same directory. Prefixes are
kept only in the pending native request and discarded after resolution. They
are not exposed in public SDK contracts or logged.

AionUI reference: `docs/prds/conversations/acp/permissions.md` defines remembered
approval for the same operation/object. Its dynamic permission panel passes the
selected native option through; the runtime is responsible for applying it.

Regression tests replay a native request through the production reader and
answer dispatcher, test allow/always/deny separately, prevent command rules
from leaking to file approvals, reject malformed/unoffered prefixes, and check
full-access -> workspace/read-only policy restoration and custom profiles.
Rust verification and both release builds run in CI; no local compilation or
Rust test suite is used. Existing config observations and permission-delivery
logs provide diagnostics without adding command or prefix payloads to logs.


Claude's native adapter now snapshots validated `permission_suggestions` and
returns `updatedPermissions` only on Allow Always, with the SDK's user-permanent
decision classification. Only additive allow rules/directories are accepted;
no tool-input-derived rule or mode change is invented. Explicit matched ask
rules, native suppression, malformed bundles and questions never acquire a
remembered approval. Live and REST-recovered cards omit Allow Always when the
native request has no supported rule. This capability stays inside ACPCore;
the plugin's public contracts and process boundaries are unchanged.

Verified: docs/protocol-schemas/claude-2.1.267/README.md, the corresponding official
SDK distributions and native two-call capture (Allow Once: two approvals; Allow
Always: one). Permission scopes and native safety checks remain authoritative;
this is not an extra bypass on top of Claude's permission mode. An initial
redirection-command probe remained interactive and is explicitly documented
as a limitation rather than reported as passing.
