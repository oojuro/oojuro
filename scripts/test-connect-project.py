#!/usr/bin/env python3
"""Synthetic subprocess boundary tests, without changing real agent settings."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("connect-project.py")
PROJECT = "a" * 64
SECRET = "ojp_synthetic_private_read_key_never_print"


class ProjectSetup(unittest.TestCase):
    def fixture(self, root):
        binary = root / "oojuro"
        binary.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
assert "OOJURO_PROJECT_READ_KEY" not in os.environ
assert "OOJURO_API_KEY" not in os.environ
args=sys.argv[1:]
with open(Path(__file__).with_name("calls.jsonl"),"a") as log:log.write(json.dumps(args)+"\\n")
command=args[1];path=Path(args[args.index("--connection")+1])
if command=="connect-project":
 key=Path(args[args.index("--read-key-file")+1]).read_text()
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
 with os.fdopen(fd,"w") as out:json.dump({"version":1,"base_url":args[args.index("--base-url")+1],"project":args[args.index("--project")+1],"read_key":key},out)
 print('{"project_read_connection_saved":true}')
elif command=="records":print('{"records":[],"snapshot":"synthetic"}')
elif command=="mcp":
 frames=[json.loads(line) for line in sys.stdin]
 assert [f["method"] for f in frames]==["initialize","notifications/initialized","tools/list"]
 print(json.dumps({"id":1,"result":{"protocolVersion":"2025-11-25","serverInfo":{"name":"oojuro-project"}}}))
 print(json.dumps({"id":2,"result":{"tools":[{"name":x} for x in ("records","search","get")]}}))
else:raise Exception("unexpected command")
''')
        binary.chmod(0o700)
        agent = root / "agent"
        agent.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
assert "OOJURO_PROJECT_READ_KEY" not in os.environ
assert "OOJURO_API_KEY" not in os.environ
args=sys.argv[1:];root=Path(__file__).parent;config=root/"agent-config.json"
with open(root/"calls.jsonl","a") as log:log.write(json.dumps(args)+"\\n")
saved=json.loads(config.read_text())
if args[1]=="list":print(json.dumps([{"name":name} for name in saved]));sys.exit(0)
if args[1]=="get":
 if args[2] in saved:sys.exit(0)
 print('No MCP server named "'+args[2]+'". Run `claude mcp add` to add one.',file=sys.stderr);sys.exit(1)
assert args[1]=="add"
name=args[2] if args[2]!="--transport" else args[6]
assert name not in saved,"must not overwrite"
saved[name]=args[args.index("--")+1:]
config.write_text(json.dumps(saved))
''')
        agent.chmod(0o700)
        (root / "codex").symlink_to(agent)
        (root / "claude").symlink_to(agent)
        (root / "agent-config.json").write_text(json.dumps({"unrelated-server": ["do-not-change"]}))
        return binary

    def test_both_agents_register_paths_only_and_preserve_existing_entries(self):
        for adapter in ("codex", "claude-code"):
            with self.subTest(adapter=adapter), tempfile.TemporaryDirectory(prefix="oojuro-project-setup-") as temporary:
                root = Path(temporary)
                binary = self.fixture(root)
                env = dict(os.environ, PATH=str(root)+os.pathsep+os.environ["PATH"], OOJURO_PROJECT_READ_KEY=SECRET, OOJURO_API_KEY="excluded-capture-secret")
                directory = root / "private"
                command = [sys.executable, str(SCRIPT), "--binary", str(binary), "--adapter", adapter,
                           "--project", PROJECT, "--directory", str(directory)]
                out = subprocess.run(command, env=env, capture_output=True, timeout=10)
                self.assertEqual(out.returncode, 0, out.stderr.decode())
                self.assertNotIn(SECRET.encode(), out.stdout+out.stderr)
                self.assertNotIn(b"excluded-capture-secret", out.stdout+out.stderr)
                report = json.loads(out.stdout.splitlines()[-1])
                self.assertTrue(report["project_read_verified"])
                self.assertFalse(report["agent_tool_call_verified"])
                self.assertFalse((directory/"setup-read-key").exists())
                connection = directory / "project.json"
                self.assertEqual(connection.stat().st_mode & 0o777, 0o600)
                config = root / "agent-config.json"
                saved = json.loads(config.read_text())
                self.assertEqual(saved["unrelated-server"], ["do-not-change"])
                self.assertEqual(saved["oojuro-history-"+PROJECT[:12]], [str(binary), "cloud", "mcp", "--connection", str(connection)])
                self.assertNotIn(SECRET, config.read_text()+(root/"calls.jsonl").read_text())
                original_config, original_connection = config.read_bytes(), connection.read_bytes()
                env.pop("OOJURO_PROJECT_READ_KEY")
                out = subprocess.run(command, env=env, capture_output=True, timeout=10)
                self.assertEqual(out.returncode, 2)
                self.assertIn(b"already exists", out.stderr)
                self.assertNotIn(SECRET.encode(), out.stdout+out.stderr)
                self.assertEqual(config.read_bytes(), original_config)
                self.assertEqual(connection.read_bytes(), original_connection)

    def test_dry_run_needs_no_key_and_changes_nothing(self):
        with tempfile.TemporaryDirectory(prefix="oojuro-project-plan-") as temporary:
            root = Path(temporary)
            binary = self.fixture(root)
            env = dict(os.environ, PATH=str(root)+os.pathsep+os.environ["PATH"])
            env.pop("OOJURO_PROJECT_READ_KEY", None)
            directory = root / "private"
            out = subprocess.run([sys.executable, str(SCRIPT), "--binary", str(binary), "--adapter", "codex",
                                  "--project", PROJECT, "--directory", str(directory), "--dry-run"], env=env, capture_output=True, timeout=10)
            self.assertEqual(out.returncode, 0, out.stderr.decode())
            self.assertFalse(directory.exists())
            self.assertFalse((root/"calls.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
