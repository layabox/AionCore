-- Startup recovery runs before HTTP readiness. Keep completed history out of
-- its access path; status first makes SQLite search this partial index instead
-- of walking every text/thinking/tool message through idx_messages_type.
CREATE INDEX IF NOT EXISTS idx_messages_stale_runtime
    ON messages(status, created_at, conversation_id)
    WHERE position = 'left'
      AND status IN ('work', 'pending')
      AND type IN ('text', 'thinking', 'tool_call', 'tool_group');
