-- Cache user-send recency on the conversation, so list readers never need to
-- page through message history. Keep the existing all-activity updated_at intact.
ALTER TABLE conversations ADD COLUMN last_user_message_at INTEGER;

-- Only user rows participate. Normalize legacy second timestamps in the index
-- and summary, matching history readers; current writers use milliseconds.
CREATE INDEX IF NOT EXISTS idx_messages_user_activity
ON messages (
    conversation_id,
    (CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END) DESC
)
WHERE (position = 'right' OR type = 'user_content') AND created_at > 0
    AND type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal');

-- This backfill runs once under the normal migration transaction. The covering
-- partial index makes each lookup a single user-history index seek, not a scan
-- of message content. The explicit index also avoids stale legacy planner
-- statistics choosing a full-history index. Empty conversations retain NULL.
UPDATE conversations
SET last_user_message_at = (
    SELECT CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END
    FROM messages INDEXED BY idx_messages_user_activity
    WHERE conversation_id = conversations.id
        AND (position = 'right' OR type = 'user_content') AND created_at > 0
        AND type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal')
    ORDER BY (CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END) DESC
    LIMIT 1
);

-- Triggers cover ordinary inserts, streaming upserts, imports and forks in the
-- same transaction as the actual persisted message. Failed writes roll back
-- both changes. Assistant/tool writes do not update user recency.
CREATE TRIGGER IF NOT EXISTS messages_user_activity_insert
AFTER INSERT ON messages
WHEN (NEW.position = 'right' OR NEW.type = 'user_content') AND NEW.created_at > 0
    AND NEW.type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal')
BEGIN
    UPDATE conversations
    SET last_user_message_at = CASE
        WHEN NEW.created_at < 10000000000 THEN NEW.created_at * 1000 ELSE NEW.created_at END
    WHERE id = NEW.conversation_id AND COALESCE(last_user_message_at, 0) <
        CASE WHEN NEW.created_at < 10000000000 THEN NEW.created_at * 1000 ELSE NEW.created_at END;
END;

-- Upserts retain the persisted timestamp (MIN on conflict), not the retry's
-- arrival time. Recompute only when a field that defines user recency changes.
CREATE TRIGGER IF NOT EXISTS messages_user_activity_update
AFTER UPDATE OF conversation_id, position, type, created_at ON messages
WHEN (OLD.conversation_id IS NOT NEW.conversation_id OR OLD.position IS NOT NEW.position
    OR OLD.type IS NOT NEW.type OR OLD.created_at IS NOT NEW.created_at)
    AND ((OLD.position = 'right' OR OLD.type = 'user_content')
        OR (NEW.position = 'right' OR NEW.type = 'user_content'))
BEGIN
    UPDATE conversations
    SET last_user_message_at = (
        SELECT CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END
        FROM messages INDEXED BY idx_messages_user_activity
        WHERE conversation_id = conversations.id
            AND (position = 'right' OR type = 'user_content') AND created_at > 0
            AND type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal')
        ORDER BY (CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END) DESC
        LIMIT 1
    )
    WHERE id IN (OLD.conversation_id, NEW.conversation_id);
END;

-- Clearing/resetting history must also clear recency. Deleting an older user
-- row does not rewrite the summary; deleting the latest seeks the next one.
CREATE TRIGGER IF NOT EXISTS messages_user_activity_delete
AFTER DELETE ON messages
WHEN (OLD.position = 'right' OR OLD.type = 'user_content') AND OLD.created_at > 0
    AND OLD.type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal')
BEGIN
    UPDATE conversations
    SET last_user_message_at = (
        SELECT CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END
        FROM messages INDEXED BY idx_messages_user_activity
        WHERE conversation_id = conversations.id
            AND (position = 'right' OR type = 'user_content') AND created_at > 0
            AND type NOT IN ('cron_trigger', 'skill_suggest', 'acp_goal')
        ORDER BY (CASE WHEN created_at < 10000000000 THEN created_at * 1000 ELSE created_at END) DESC
        LIMIT 1
    )
    WHERE id = OLD.conversation_id AND last_user_message_at =
        CASE WHEN OLD.created_at < 10000000000 THEN OLD.created_at * 1000 ELSE OLD.created_at END;
END;
