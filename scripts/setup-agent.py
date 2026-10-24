#!/usr/bin/env python3
"""Set up new-activity capture and optional project-history MCP in one command.

Capture and reads keep their separate grants. Existing setup helpers own file,
credential, transcript-consent and agent-registration checks.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter", choices=("codex", "claude-code"), required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--name")
    parser.add_argument("--base-url", default="https://oojuro.com")
    parser.add_argument("--capture-directory", type=Path)
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--api-key-file", type=Path)
    parser.add_argument("--project", help="optional project whose attached history this agent may read")
    parser.add_argument("--project-directory", type=Path)
    parser.add_argument("--read-key-file", type=Path)
    parser.add_argument("--accept-local-capture", action="store_true")
    parser.add_argument("--no-service", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if not args.project and (args.read_key_file or args.project_directory):
        parser.error("--read-key-file and --project-directory require --project")
    if not args.dry_run and not args.accept_local_capture:
        parser.error("review --dry-run, then supply --accept-local-capture")

    scripts = Path(__file__).resolve().parent
    common = ["--adapter", args.adapter, "--base-url", args.base_url]
    if args.binary:
        common.extend(["--binary", str(args.binary.absolute())])
    capture = [sys.executable, str(scripts / "connect-agent.py"), *common]
    for option, value in [("--name", args.name), ("--directory", args.capture_directory),
                          ("--source-root", args.source_root), ("--api-key-file", args.api_key_file)]:
        if value is not None:
            capture.extend([option, str(value)])
    if args.no_service:
        capture.append("--no-service")
    project = None
    if args.project:
        project = [sys.executable, str(scripts / "connect-project.py"), *common,
                   "--project", args.project]
        for option, value in [("--directory", args.project_directory), ("--read-key-file", args.read_key_file)]:
            if value is not None:
                project.extend([option, str(value)])

    def run(command, allowed_key=None):
        env = os.environ.copy()
        for key in ("OOJURO_API_KEY", "OOJURO_PROJECT_READ_KEY"):
            if key != allowed_key:
                env.pop(key, None)
        # Keep interactive hidden-key input and the helpers' safe progress output.
        # Their subprocess deadlines/unknown-outcome handling remain authoritative.
        return subprocess.call(command, env=env) == 0

    # Both plans must validate before either helper can write or contact a server.
    for command in (capture, project):
        if command is not None and not run([*command, "--dry-run"]):
            print(json.dumps({"setup_incomplete": True, "phase": "preflight",
                              "capture_helper_completed": False, "project_helper_completed": False}))
            return 2
    if args.dry_run:
        return 0
    if not run([*capture, "--accept-local-capture"], "OOJURO_API_KEY"):
        print(json.dumps({"setup_incomplete": True, "phase": "capture",
                          "capture_helper_completed": False, "project_helper_completed": False,
                          "next": "Inspect capture status and preserve its connection/spool; project setup was not started."}))
        return 2
    if project is not None and not run(project, "OOJURO_PROJECT_READ_KEY"):
        print(json.dumps({"setup_incomplete": True, "phase": "project",
                          "capture_helper_completed": True, "project_helper_completed": False,
                          "next": "Capture setup completed and may be running. Preserve it; finish connect-project.py separately after resolving access or an existing MCP entry."}))
        return 2
    print(json.dumps({"capture_helper_completed": True, "project_helper_completed": project is not None,
                      "next": "Use the capture helper's delivery status. If history was configured, reconnect the agent and call records, then get an original; registration alone does not verify a client tool call."}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.SubprocessError):
        print("Setup interrupted; inspect both local helper states before retrying. Existing capture or project setup may have completed.", file=sys.stderr)
        raise SystemExit(2)
