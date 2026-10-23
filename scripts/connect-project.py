#!/usr/bin/env python3
"""Configure read-only Oojuro project history in Codex or Claude Code.

The separate project read key stays in a protected local file. Capture setup,
local transcript access and project membership are unchanged.
"""
import argparse
import getpass
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import warnings


def private_directory(path):
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("absolute private directory required")
    for part in reversed([path, *path.parents]):
        if not part.exists():
            part.mkdir(mode=0o700)
        info = part.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid()):
            raise ValueError("unsafe directory")
        if info.st_mode & 0o022 and not (info.st_uid == 0 and info.st_mode & stat.S_ISVTX):
            raise ValueError("writable directory ancestor")
    if path.stat().st_uid != os.getuid() or stat.S_IMODE(path.stat().st_mode) != 0o700:
        raise ValueError("mode 0700 directory required")


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=shutil.which("oojuro"))
    parser.add_argument("--adapter", choices=("codex", "claude-code"), required=True)
    parser.add_argument("--project", required=True)
    parser.add_argument("--read-key-file", type=Path, help="mode-0600 plaintext project read key; otherwise OOJURO_PROJECT_READ_KEY or hidden terminal input")
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--base-url", default="https://oojuro.com")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if len(args.project) != 64 or any(c not in "0123456789abcdef" for c in args.project):
        raise ValueError("invalid project ID")
    if not args.binary:
        raise ValueError("install the CLI first or supply --binary")
    binary = Path(args.binary).absolute()
    info = binary.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022 or not os.access(binary, os.X_OK):
        raise ValueError("owned, non-writable executable required")
    directory = args.directory or Path.home() / ".local/state/oojuro/projects" / args.project
    connection = directory / "project.json"
    agent = shutil.which("codex" if args.adapter == "codex" else "claude")
    if not agent:
        raise ValueError("selected agent CLI is not installed")
    name = "oojuro-history-" + args.project[:12]
    launcher = [str(binary), "cloud", "mcp", "--connection", str(connection)]
    command = ([agent, "mcp", "add", name, "--", *launcher] if args.adapter == "codex" else
               [agent, "mcp", "add", "--transport", "stdio", "--scope", "user", name, "--", *launcher])
    print(json.dumps({"plan": {"adapter": args.adapter, "project": args.project, "connection": str(connection),
                                "server": name, "access": "current project's attached journals, including earlier sessions",
                                "capture": "separate credential and setup", "register_command": command}}), flush=True)
    if args.dry_run:
        return
    os.umask(0o077)
    private_directory(directory)
    env = os.environ.copy()
    env.pop("OOJURO_PROJECT_READ_KEY", None)
    env.pop("OOJURO_API_KEY", None)
    if args.base_url.startswith("http://"):
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            env.pop(key, None)

    def run(argv, input=None, timeout=75):
        result = subprocess.run(argv, input=input, capture_output=True, timeout=timeout, env=env)
        if result.returncode:
            raise RuntimeError("setup step failed; existing files and agent entries were preserved; inspect local access/configuration")
        return result.stdout

    if not connection.exists():
        key_file = args.read_key_file
        temporary = None
        if key_file is None:
            key = os.environ.get("OOJURO_PROJECT_READ_KEY")
            if key is None:
                if not sys.stdin.isatty():
                    raise ValueError("provide --read-key-file or OOJURO_PROJECT_READ_KEY; keep keys out of agent chat")
                with warnings.catch_warnings():
                    warnings.simplefilter("error", getpass.GetPassWarning)
                    try:
                        key = getpass.getpass("Oojuro project read key (hidden): ")
                    except getpass.GetPassWarning:
                        raise ValueError("hidden input unavailable; use a protected read key file")
            temporary = directory / "setup-read-key"
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "wb") as file:
                file.write(key.strip().encode())
                file.flush()
                os.fsync(file.fileno())
            sync_directory(directory)
            key = None
            key_file = temporary
        try:
            run([str(binary), "cloud", "connect-project", "--connection", str(connection), "--project", args.project,
                 "--base-url", args.base_url, "--read-key-file", str(key_file.absolute())])
        finally:
            if temporary is not None:
                temporary.unlink()
                sync_directory(directory)
    # The CLI validates permissions and every connection field before reading metadata.
    # Check selectors as well, so a rerun cannot silently reuse another project's file.
    fd = os.open(connection, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as file:
        info = os.fstat(file.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > 8192:
            raise ValueError("protected project connection required")
        raw = file.read(8193)
    if len(raw) > 8192:
        raise ValueError("project connection too large")
    saved = json.loads(raw)
    if not isinstance(saved, dict) or saved.get("project") != args.project or saved.get("base_url") != args.base_url:
        raise ValueError("existing project connection differs; choose a separate directory")
    del saved
    page = json.loads(run([str(binary), "cloud", "records", "--connection", str(connection), "--limit", "1"]))
    if not isinstance(page.get("records"), list):
        raise RuntimeError("invalid project metadata reply")
    del page
    frames = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "oojuro-setup", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    replies = [json.loads(line) for line in run(launcher, input=("\n".join(json.dumps(f) for f in frames) + "\n").encode()).splitlines()]
    if len(replies) != 2 or [r.get("id") for r in replies] != [1, 2] or replies[0].get("result", {}).get("protocolVersion") != "2025-11-25" or replies[0].get("result", {}).get("serverInfo", {}).get("name") != "oojuro-project" or {
        t.get("name") for t in replies[1].get("result", {}).get("tools", [])
    } != {"records", "search", "get"}:
        raise RuntimeError("unexpected MCP protocol or tool scope")
    # shortcut: agent CLIs lack atomic add-if-absent; do not edit MCP config concurrently.
    # Codex's add can replace entries, so inspect its complete machine-readable inventory.
    if args.adapter == "codex":
        inventory = json.loads(run([agent, "mcp", "list", "--json"]))
        if not isinstance(inventory, list) or any(not isinstance(item, dict) or not isinstance(item.get("name"), str) for item in inventory):
            raise RuntimeError("unrecognized agent MCP inventory; configuration preserved")
        if any(item["name"] == name for item in inventory):
            raise ValueError("MCP server already exists; inspect it with the agent CLI; configuration preserved")
    else:
        # Claude's add refuses an existing user entry; get also detects higher-precedence scopes.
        existing = subprocess.run([agent, "mcp", "get", name], capture_output=True, timeout=75, env=env)
        if existing.returncode == 0:
            raise ValueError("MCP server already exists; inspect it with the agent CLI; configuration preserved")
        if existing.returncode != 1 or existing.stdout or existing.stderr.strip() != f'No MCP server named "{name}". Run `claude mcp add` to add one.'.encode():
            raise RuntimeError("unrecognized agent MCP lookup; configuration preserved")
    run(command)
    print(json.dumps({"project_read_verified": True, "mcp_protocol_verified": True, "server_registered": name,
                      "agent_tool_call_verified": False, "next": "Restart or reconnect the agent MCP client, call records, then open an original with get. Capture uses its separate setup."}))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2)
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
        print("setup failed; preserve protected files and inspect local configuration/access", file=sys.stderr)
        raise SystemExit(2)
