# Native Claude remembered permission evidence

Verified on 2026-11-09 with installed Claude Code 2.1.267. Both ephemeral
stream-json runs requested two sequential identical `touch permission-probe.txt`
Bash calls in a temporary working directory. Allow Once produced two approvals;
Allow Always returned the CLI's exact `permission_suggestions` as
`updatedPermissions` and produced only one approval. Both tools executed and
the turn completed successfully. The permanent response includes the official
`decisionClassification: user_permanent`; a separate same-command probe without
that optional field also produced one approval.

The captured frames contain only this probe's own commands. Temporary paths
were normalized to `/tmp/claude-permission-probe` consistently. No credentials,
user prompts or project files are included. Temporary directories, local policy
files and subprocess groups were removed after the probes. Session persistence
was disabled. These are native CLI protocol probes, not local Rust tests/builds.

Primary sources read for the implementation:

- Official SDK 0.3.267 `sdk.d.ts` and `sdk.mjs`, distributed by
  `@anthropic-ai/claude-agent-sdk`: PermissionResult, PermissionUpdate,
  `permission_suggestions`, `matched_ask_rule`, and decisionClassification.
- Official SDK 0.3.268 `sdk.mjs`: raw `suppress_always_allow_rule` mapping.
- https://code.claude.com/docs/en/agent-sdk/typescript#canusetool
- https://github.com/anthropics/claude-agent-sdk-python/tree/2209f2c134fee914c0f75733b631a5fc238ca846
  (`types.py` and `_internal/query.py`): suggestion deserialization and exact
  `updatedPermissions` serialization in the stdio response.
- https://github.com/zed-industries/claude-agent-acp/tree/3b22586cf63cef06f1029a8418a9abb3b2cc3236/src/permissions
  (`normalization.ts`, `effects.ts`, `response.ts`): validate and snapshot the
  native bundle, respect forced asks and native suppression, distinguish
  temporary/permanent/rejected decisions.

ACPCore accepts only additive allow rules/directories, validates the complete
bundle, and keeps native destinations and rule content unchanged. A malformed
bundle, explicit ask rule, suppression flag, question, mode switch, removal or
replacement does not produce a remembered-approval option. Live and recovered
cards share this request-specific capability; raw policy updates stay private.

Scope limit: an initial `printf` command with shell redirection still prompted
twice despite echoing the native proposed bundle. That sample is not described
as a successful remembered approval. The passing comparison uses an ordinary
file-creation command. Native safety and rule-matching checks remain authoritative;
ACPCore does not implement an extra blanket permission bypass.
