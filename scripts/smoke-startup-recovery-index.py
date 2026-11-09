"""Exercise the packaged executable's upgrade/recovery in a disposable database."""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
args = parser.parse_args()
binary = args.binary.resolve()

with tempfile.TemporaryDirectory(prefix="acpcore-recovery-index-") as directory:
    root = Path(directory)
    data = root / "data"
    db_path = data / "aionui-backend.db"
    child = None
    output = None

    def stop():
        global child, output
        if child is not None:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
            child = None
        if output is not None:
            output.close()
            output = None

    def start():
        global child, output
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        output = (root / "process-output.log").open("w")
        child = subprocess.Popen([
            str(binary), "--host", "127.0.0.1", "--port", str(port),
            "--data-dir", str(data), "--log-dir", str(root / "logs"),
            "--work-dir", str(root), "--parent-pid", str(os.getpid()),
            "--local", "--managed-resources-mode", "download",
        ], cwd=root, stdout=output, stderr=subprocess.STDOUT)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and child.poll() is None:
            try:
                with opener.open(f"http://127.0.0.1:{port}/health", timeout=1) as response:
                    if response.status == 200:
                        return
            except (urllib.error.URLError, TimeoutError):
                pass
            time.sleep(0.1)
        raise RuntimeError((root / "process-output.log").read_text(errors="replace")[-4000:])

    try:
        start()
        stop()
        with closing(sqlite3.connect(db_path)) as db, db:
            db.execute("DROP INDEX idx_messages_stale_runtime")
            db.execute("DELETE FROM _sqlx_migrations WHERE version = 46")
            db.execute("INSERT INTO conversations (id,user_id,name,type,created_at,updated_at) "
                       "VALUES ('index-smoke','system_default_user','Index smoke','codex',1,1)")
            db.executemany("INSERT INTO messages (id,conversation_id,type,content,position,status,created_at) "
                           "VALUES (?,'index-smoke','text',?,'left','finish',?)",
                           [(f"history-{i}", json.dumps({"content": f"history {i}"}), i) for i in range(256)])
            db.execute("INSERT INTO messages (id,conversation_id,type,content,position,status,created_at) "
                       "VALUES ('active','index-smoke','text','{\"content\":\"unfinished output\"}','left','work',300)")
            db.execute("INSERT INTO messages (id,conversation_id,type,content,position,status,created_at) "
                       "VALUES ('user-pending','index-smoke','text','{}','right','pending',301)")
            history = db.execute("SELECT id,content,status FROM messages WHERE id LIKE 'history-%' ORDER BY id").fetchall()
        start()
        stop()
        with closing(sqlite3.connect(db_path)) as db, db:
            assert db.execute("SELECT count(*) FROM sqlite_master WHERE name='idx_messages_stale_runtime'").fetchone()[0] == 1
            migration = db.execute("SELECT * FROM _sqlx_migrations WHERE version=46 AND success=1").fetchall()
            assert len(migration) == 1
            assert db.execute("SELECT status FROM messages WHERE id='active'").fetchone()[0] == "finish"
            assert db.execute("SELECT status FROM messages WHERE id='user-pending'").fetchone()[0] == "pending"
            assert db.execute("SELECT id,content,status FROM messages WHERE id LIKE 'history-%' ORDER BY id").fetchall() == history
            version = db.execute("PRAGMA schema_version").fetchone()[0]
        start()
        stop()
        with closing(sqlite3.connect(db_path)) as db, db:
            assert db.execute("SELECT * FROM _sqlx_migrations WHERE version=46 AND success=1").fetchall() == migration
            assert db.execute("PRAGMA schema_version").fetchone()[0] == version
            assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
        print("Packaged runtime: existing database upgraded once; stale output recovered; history and pending user input preserved; restart skipped index rebuild.")
    finally:
        stop()
