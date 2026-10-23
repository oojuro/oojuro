#!/usr/bin/env python3
"""Private preview: browser-approved capture, then a per-user Linux service.

No secrets on argv, stdout, in prompts or URLs. Existing history is excluded.
A setup marker proves delivery; it does not prove real transcript capture.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import time


def private_dir(path):
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("absolute directory required")
    for part in reversed([path, *path.parents]):
        if not part.exists():
            part.mkdir(mode=0o700)
        info = part.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid()):
            raise ValueError("unsafe directory")
        if info.st_mode & 0o022 and not (info.st_uid == 0 and info.st_mode & stat.S_ISVTX):
            raise ValueError("writable directory ancestor")
    if path.stat().st_uid != os.getuid() or stat.S_IMODE(path.stat().st_mode) != 0o700:
        raise ValueError("mode 0700 installation directory required")


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def private_write(path, content, mode=0o600):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    with os.fdopen(fd, "wb") as file:
        file.write(content)
        file.flush()
        os.fsync(file.fileno())
    sync_dir(path.parent)


def unit_quote(value):
    if any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError("control character in service argument")
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--journal", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--adapter", choices=("codex", "claude-code"), required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--base-url", default="https://oojuro.com")
    parser.add_argument("--accept-local-capture", action="store_true")
    parser.add_argument("--no-service", action="store_true", help="connect and verify, leaving capture stopped")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if sys.platform != "linux":
        raise ValueError("preview installer currently supports Linux; use the CLI foreground command on macOS")
    source_root = args.source_root or Path.home() / (".codex/sessions" if args.adapter == "codex" else ".claude/projects")
    if not source_root.is_absolute() or not source_root.is_dir() or source_root.is_symlink():
        raise ValueError("supported transcript root is unavailable; specify --source-root")
    if any(len(value) != 64 or any(c not in "0123456789abcdef" for c in value) for value in (args.source, args.journal)):
        raise ValueError("invalid journal identifiers")
    binary = args.binary.absolute()
    info = binary.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise ValueError("owned, non-writable CLI binary required")
    plan = dict(adapter=args.adapter, transcript_root=str(source_root), installation=str(args.directory),
                journal=args.journal, capture="new activity only", service=not args.no_service,
                excluded="existing history, referenced tool-result files, encrypted reasoning and non-transcript activity")
    print(json.dumps({"plan": plan}), flush=True)
    if args.dry_run:
        return
    if not args.accept_local_capture:
        raise ValueError("review --dry-run, then supply --accept-local-capture")
    os.umask(0o077)
    private_dir(args.directory)
    installed = args.directory / "oojuro"
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    if installed.exists():
        saved = installed.lstat()
        if not stat.S_ISREG(saved.st_mode) or saved.st_uid != os.getuid() or stat.S_IMODE(saved.st_mode) != 0o700 or hashlib.sha256(installed.read_bytes()).hexdigest() != digest:
            raise ValueError("installed binary differs; inspect it before upgrading")
    else:
        private_write(installed, binary.read_bytes(), mode=0o700)
    connection = args.directory / "connection.json"
    env = os.environ.copy()
    # Explicit loopback fixtures need no proxy. HTTPS production retains normal TLS/proxy checks.
    if args.base_url.startswith("http://"):
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"):
            env.pop(key, None)

    def cli(*arguments, timeout=40, allow_pending=False):
        result = subprocess.run([str(installed), "--home", str(args.directory / "unused-local-home"),
                                 "cloud", *arguments, "--connection", str(connection)],
                                capture_output=True, timeout=timeout, env=env)
        if result.returncode:
            if allow_pending and b"capture_not_authorized_or_expired" in result.stderr:
                return None
            # Never relay provider errors or private configuration to an agent/chat.
            raise RuntimeError("CLI setup step failed; inspect the local capture status")
        return json.loads(result.stdout)

    connected = None
    if connection.exists():
        connected = cli("capture-status", allow_pending=True)
    if not connected:
        approval = (cli("connection") if connection.exists() else
                    cli("connect", "--base-url", args.base_url, "--source", args.source,
                        "--journal", args.journal, "--label", args.name))
        print(json.dumps({"browser_approval": approval, "instruction": "Open this URL on the signed-in account, compare the fingerprint, and approve this journal connection."}), flush=True)
        deadline = time.monotonic() + min(600, max(0, (approval["expires_us"] / 1_000_000) - time.time()))
        while time.monotonic() < deadline:
            connected = cli("capture-status", allow_pending=True)
            if connected:
                break
            time.sleep(3)
        if not connected:
            raise RuntimeError("approval expired; keep the private file and restart with a fresh connection file")
    if connected["journal"] != args.journal:
        raise ValueError("existing connection is bound to another journal")
    spool = args.directory / "spool"
    capture = ["capture", "--state", str(spool), "--source-root", str(source_root),
               "--adapter", args.adapter, "--accept-local-capture"]
    initialized = cli(*capture, "--once", timeout=180)
    check = cli("capture-check")
    result = dict(journal=args.journal, adapter=args.adapter, binary_sha256=digest,
                  connection_check_accepted=check["connection_check_accepted"],
                  real_history_verified=False, delivered_transcript_events=initialized["delivered"],
                  manual_capture_command=[str(installed), "cloud", *capture, "--connection", str(connection)])
    if not args.no_service:
        config_root = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
        unit_dir = config_root / "systemd/user"
        private_dir(args.directory)  # Validate protected paths again before installing the service.
        # Existing user systemd directories may be 0755; they contain no capture secret.
        if not config_root.is_absolute() or ".." in config_root.parts:
            raise ValueError("absolute user service directory required")
        for parent in reversed([unit_dir, *unit_dir.parents]):
            if not parent.exists():
                parent.mkdir(mode=0o700)
            info = parent.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid not in (0, os.getuid()) or info.st_mode & 0o022:
                raise ValueError("unsafe user service directory")
        unit_name = f"oojuro-capture-{args.journal[:16]}-{args.adapter}.service"
        argv = [str(installed), "cloud", *capture, "--connection", str(connection)]
        unit = ("[Unit]\nDescription=Oojuro automatic agent history capture\nAfter=network-online.target\n"
                "StartLimitIntervalSec=300\nStartLimitBurst=10\n[Service]\nType=simple\nExecStart=" +
                " ".join(unit_quote(value) for value in argv) +
                "\nRestart=on-failure\nRestartSec=30\nUMask=0077\nNice=15\nMemoryMax=512M\nCPUQuota=50%\n"
                "NoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=read-only\nReadWritePaths=" + unit_quote(str(spool)) +
                "\n[Install]\nWantedBy=default.target\n").encode()
        target = unit_dir / unit_name
        if target.exists():
            saved = target.lstat()
            if not stat.S_ISREG(saved.st_mode) or saved.st_uid != os.getuid() or saved.st_mode & 0o022 or target.read_bytes() != unit:
                raise ValueError("existing service differs; inspect it before updating")
        else:
            private_write(target, unit)
        subprocess.run(["systemctl", "--user", "daemon-reload"], check=True, timeout=15, capture_output=True)
        subprocess.run(["systemctl", "--user", "enable", "--now", unit_name], check=True, timeout=20, capture_output=True)
        subprocess.run(["systemctl", "--user", "is-active", "--quiet", unit_name], check=True, timeout=10, capture_output=True)
        result["service"] = unit_name
        result["stop_command"] = ["systemctl", "--user", "disable", "--now", unit_name]
    receipt = args.directory / "setup-receipt.json"
    if not receipt.exists():
        private_write(receipt, json.dumps(result, indent=2).encode())
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        # Our static errors carry no key/payload/provider response.
        print("Oojuro setup incomplete: " + (str(error) if isinstance(error, (ValueError, RuntimeError)) else "local setup operation failed"), file=sys.stderr)
        sys.exit(1)
