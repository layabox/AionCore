# Codex asynchronous question evidence

Generated with installed codex-cli 0.164.0-alpha.11.1 using `app-server generate-json-schema --experimental`. AsyncAgentMessage.json extracts exact relevant definitions from ServerNotification.json.

Synthetic live capture: requested one layout question (Compact/Spacious). The CLI emitted the same `agentMessage` with `delivery: async` and structured `questions` at item/started and item/completed, then completed the turn. A second `turn/start` user message selected Spacious; the CLI replied Spacious. No native reverse-RPC question response was sent. Input contained no project data.

Question answers must therefore use the normal conversation message service, including mid-turn steering, history, runtime claims and relay. Local acknowledgement only retires the card; decline does not send a fabricated reverse-RPC or select an answer.
