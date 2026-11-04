const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const fs = require("node:fs/promises");
const { createServer } = require("node:net");
const os = require("node:os");
const path = require("node:path");
const binary = path.resolve(process.argv[2] ?? path.join(
  __dirname, "../assets/layaair-agent/runtime/acp-core",
  process.platform === "win32" ? "win32-x64/aioncore.exe" : "darwin-arm64/aioncore",
));
let child;
async function stop() {
  if (child?.exitCode === null && child.signalCode === null) {
    child.kill();
    await once(child, "exit");
  }
}
const data = (project) => path.join(project, "local/layaair-agent/acp-core/data");
async function start(project) {
  const socket = createServer().listen(0, "127.0.0.1");
  await once(socket, "listening");
  const { port } = socket.address();
  await new Promise((resolve) => socket.close(resolve));
  const base = `http://127.0.0.1:${port}`;
  child = spawn(binary, ["--host", "127.0.0.1", "--port", String(port), "--data-dir", data(project),
    "--log-dir", path.join(project, "logs"), "--work-dir", project, "--parent-pid", String(process.pid),
    "--app-version", "0.1.0", "--local", "--managed-resources-mode", "download"], { cwd: project });
  let logs = "";
  let startError;
  child.on("error", (error) => { startError = error; });
  for (const stream of [child.stdout, child.stderr]) stream.on("data", (data) => {
    logs = (logs + data).slice(-8000);
  });
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    if (startError) throw startError;
    if ((await fetch(base + "/health", { signal: AbortSignal.timeout(1_000) }).catch(() => null))?.ok) {
      return async (route, body) => {
        const response = await fetch(base + route, {
          signal: AbortSignal.timeout(30_000),
          ...(body === undefined ? {} : {
            method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
          }),
        });
        const decoded = await response.json();
        assert(response.ok, `${route}: ${JSON.stringify(decoded)}`);
        return decoded.data ?? decoded;
      };
    }
    if (child.exitCode !== null || child.signalCode !== null) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(logs);
}

(async () => {
  const temp = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "laya-channel-guard-")));
  const original = path.join(temp, "old-project");
  const current = path.join(temp, "new-project");
  const skill = "relocated-review-skill";
  const source = path.join(temp, "skill-package");
  const expected = (project) => path.join(data(project), "skills/users/system_default_user", skill);
  try {
    await fs.mkdir(original, { recursive: true });
    await fs.mkdir(source);
    await fs.writeFile(path.join(source, "SKILL.md"), `---\nname: ${skill}\ndescription: relocation check\n---\n`);
    let request = await start(original);
    await request("/api/skills/import", { skill_path: source });
    const agents = await request("/api/agents/management");
    const agent = agents.find((item) => item.id === "codex") ?? agents[0];
    assert(agent, "ACPCore must expose at least one managed Agent");
    const existing = [];
    for (const channel of ["lark", "weixin"]) {
      existing.push(await request("/api/conversations", {
        source: channel, name: channel,
        assistant: { id: `bare:${agent.id}`, conversation_overrides: { skill_ids: [skill] } },
        extra: { workspace: original, custom_workspace: true },
      }));
    }
    await stop();
    // Keep the old Skill on disk and carry the existing conversations and catalog into the moved project.
    await fs.mkdir(data(current), { recursive: true });
    await fs.copyFile(path.join(data(original), "aionui-backend.db"), path.join(data(current), "aionui-backend.db"));
    const wal = "aionui-backend.db-wal";
    await fs.copyFile(path.join(data(original), wal), path.join(data(current), wal)).catch((error) => {
      if (error.code !== "ENOENT") throw error;
    });
    request = await start(current);
    assert.equal((await request("/api/skills")).find((row) => row.name === skill).location, expected(original));
    for (const session of existing) {
      const history = await request(`/api/conversations/${session.id}`);
      assert.equal(history.source, session.source);
      assert(history.extra.skills.includes(skill));
      assert.deepEqual((await request("/api/skills/materialize-for-agent", {
        conversation_id: session.id, skills: [skill],
      })).skills, []);
    }
    await request("/api/skills/import", { skill_path: source });
    for (const session of existing) {
      assert.deepEqual((await request("/api/skills/materialize-for-agent", {
        conversation_id: session.id, skills: [skill],
      })).skills, [{ name: skill, source_path: expected(current) }]);
    }
    console.log("Patched ACPCore rejects the old catalog path in both existing channel conversations and accepts the restored current path.");
  } finally {
    await stop();
    await fs.rm(temp, { recursive: true, force: true, maxRetries: 3 });
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
