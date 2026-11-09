# Claude native model discovery evidence

Captured from 2.1.267 (Claude Code) using a temporary cwd, no prompt and no session persistence. The CLI read the operator's existing model mappings. Only the public model catalog was retained; commands and all other initialization fields were omitted. No credentials or local paths are included.

Request: `{"type": "control_request", "request_id": "catalog-capture", "request": {"subtype": "initialize"}}`.

Arguments: `--print --input-format stream-json --output-format stream-json --verbose --no-session-persistence --strict-mcp-config --mcp-config {"mcpServers":{}}`; child environment adds `CLAUDE_CODE_REMOTE=1`, matching the native ACPCore connection. The process was terminated after initialize. This verifies the advertised model directory, not inference access or model availability at the provider.
