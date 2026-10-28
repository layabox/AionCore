"""Verify release architecture and record the exact cloud-built source and binary."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--platform", required=True, choices=["darwin-arm64", "win32-x64"])
parser.add_argument("--target", required=True)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
source = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
assert source == os.environ["GITHUB_SHA"]
subprocess.run(["git", "diff", "--exit-code"], check=True)
with args.binary.open("rb") as stream:
    header = stream.read(64)
    if args.platform == "darwin-arm64":
        assert args.target == "aarch64-apple-darwin"
        assert header[:4] == b"\xcf\xfa\xed\xfe"
        assert struct.unpack_from("<I", header, 4)[0] == 0x0100000C
    else:
        assert args.target == "x86_64-pc-windows-msvc"
        assert header[:2] == b"MZ"
        stream.seek(struct.unpack_from("<I", header, 0x3C)[0])
        pe = stream.read(6)
        assert pe[:4] == b"PE\0\0" and struct.unpack_from("<H", pe, 4)[0] == 0x8664
provenance = {
    "schemaVersion": 1,
    "repository": os.environ["GITHUB_REPOSITORY"],
    "sourceCommit": source,
    "workflowCommit": os.environ["GITHUB_SHA"],
    "workflowRun": "https://github.com/" + os.environ["GITHUB_REPOSITORY"] + "/actions/runs/" + os.environ["GITHUB_RUN_ID"],
    "platform": args.platform,
    "target": args.target,
    "compiler": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "binarySha256": hashlib.file_digest(args.binary.open("rb"), "sha256").hexdigest(),
}
args.output.mkdir(parents=True, exist_ok=True)
shutil.copy2(args.binary, args.output / args.binary.name)
(args.output / "BUILD_PROVENANCE.json").write_text(json.dumps(provenance, indent=2) + "\n")
print(json.dumps(provenance, indent=2))
