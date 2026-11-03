# Native MCP tool approval capture

Captured on 2026-11-03 from codex-cli 0.166.0-alpha.5.1, launched through the shipped ACPCore 0.2.2 fork. A task-owned stdio MCP server exposed only a synthetic `get_probe_value` tool without annotations. Its result was `MCP_IDENTITY_OK`; it did not read or change project files.

The native `mcpServer/elicitation/request` carries `serverName`, `mode: form`, `_meta.codex_approval_kind: mcp_tool_call`, and an empty `requestedSchema.properties`. The baseline live permission frame retained the Server but `GET /confirmations` omitted it both before and after WebSocket reconnect. The request was answered with the single-call allow option and the turn completed. Global Codex configuration was unchanged. All identifiers and content in this capture belong to the synthetic test.
