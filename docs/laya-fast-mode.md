# LayaAir Agent Fast mode patch

Base: iOfficeAI/AionCore v0.2.1, a9e113437a5cf0552075de618ee9458be24ef756.

The patch keeps agent connections inside ACPCore. Codex speed is exposed as a
select option with id `service_tier` and category `speed`. The model's actual
`model/list.data[].serviceTiers` supplies the nonstandard choices. Standard is
represented by `standard` and maps to `thread/settings/update.serviceTier: null`.
The observed `default` response maps back to `standard`. No speed capability is
fabricated for CLIs that omit serviceTiers. An advertised empty list offers only
Standard, so switching models removes unsupported Fast choices.

Protocol verification: Codex CLI 0.161.0 generated v2/ModelListResponse.json,
v2/ThreadSettingsUpdateParams.json and v2/ThreadSettingsUpdatedNotification.json.
A live ephemeral thread on 2026-10-26 returned serviceTier `default` on start,
`priority` after setting Fast, and `default` after clearing it. No inference turn
was sent during this protocol probe.

Config writes wait for a newer matching thread-settings observation. A write or
RPC acknowledgement alone never confirms a speed change. Failure remains visible.
Confirmed values persist in session config selections; creation takes the frozen
`extra.pending_config_options.service_tier`. Both are applied before registering
the session task, so a failed speed selection cannot silently run the first turn
with another tier. Runtime notices/errors remain English.

Verification:

```sh
cargo test -p aionui-session service_tier --locked
cargo check -p aionui-ai-agent --locked
cargo build --release -p aionui-app --locked
```

The session tests replay the observed wire notification and verify Fast's actual
wire id, Standard's null reset, waiting for observation, and model-specific lists.

Additional verification: a real ACPCore HTTP conversation was created with a
specific model and initial Fast selection. Standard/Fast switching confirmed
observed values, and after one minimal inference turn a backend restart restored
the last confirmed Standard selection ahead of the original Fast seed.

Startup waits for the requested model's observation before selecting speed.
An already observed target value is idempotent (no second write/notification is
required). Starting or waking a connection clears stale service-tier observations;
resume must finish before a speed write can trust its pre-seeded thread id.
