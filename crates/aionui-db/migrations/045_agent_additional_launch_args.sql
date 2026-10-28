-- User-owned launch arguments are independent of registry seeds and command/env overrides.
ALTER TABLE agent_metadata ADD COLUMN additional_args TEXT;
