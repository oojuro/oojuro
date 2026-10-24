#!/usr/bin/env python3
"""Run both real Python helpers with synthetic CLI/agent boundaries only."""
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("setup-agent.py")
PROJECT = "a" * 64
CAPTURE_KEY = "synthetic-capture-secret-not-output"
READ_KEY = "synthetic-project-secret-not-output"


class CombinedSetup(unittest.TestCase):
    def fixture(self, root, adapter="codex"):
        # Retain the existing agent-configuration fixture's add-if-absent behavior.
        fixture = runpy.run_path(str(SCRIPT.with_name("test-connect-project.py")))["ProjectSetup"]()
        binary = fixture.fixture(root)
        binary.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
assert "OOJURO_API_KEY" not in os.environ
assert "OOJURO_PROJECT_READ_KEY" not in os.environ
args=sys.argv[1:];command=args[args.index("cloud")+1]
root=Path(__file__).parent;path=Path(args[args.index("--connection")+1])
with open(root/"calls.jsonl","a") as log:log.write(json.dumps(args)+"\\n")
if command in ("connect-api","connect-project"):
 flag="--api-key-file" if command=="connect-api" else "--read-key-file"
 key=Path(args[args.index(flag)+1]).read_text()
 value={"version":2,"integration_key":key} if command=="connect-api" else {"version":1,"read_key":key,"project":args[args.index("--project")+1],"base_url":args[args.index("--base-url")+1]}
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
 with os.fdopen(fd,"w") as out:json.dump(value,out)
 result={"saved":True}
elif command=="capture-status":result={"journal_mode":"session"}
elif command=="capture":
 if (root/"capture-fails").exists():sys.exit(1)
 result={"delivered":0}
elif command=="records":
 if (root/"project-fails").exists():sys.exit(1)
 result={"records":[]}
elif command=="mcp":
 frames=[json.loads(line) for line in sys.stdin]
 assert [frame["method"] for frame in frames]==["initialize","notifications/initialized","tools/list"]
 print(json.dumps({"id":1,"result":{"protocolVersion":"2025-11-25","serverInfo":{"name":"oojuro-project"}}}))
 print(json.dumps({"id":2,"result":{"tools":[{"name":name} for name in ("records","search","get")]}}))
 sys.exit(0)
else:raise Exception("unexpected command")
print(json.dumps(result))
''')
        transcript = root / "transcripts"
        transcript.mkdir(mode=0o700)
        env = dict(os.environ, PATH=str(root)+os.pathsep+os.environ["PATH"],
                   OOJURO_API_KEY=CAPTURE_KEY, OOJURO_PROJECT_READ_KEY=READ_KEY)
        command = [sys.executable, str(SCRIPT), "--adapter", adapter, "--binary", str(binary),
                   "--capture-directory", str(root/"capture"), "--source-root", str(transcript),
                   "--project", PROJECT, "--project-directory", str(root/"project"), "--no-service"]
        return command, env

    def run_setup(self, command, env, *extra):
        result = subprocess.run([*command, *extra], env=env, capture_output=True, timeout=15)
        self.assertNotIn(CAPTURE_KEY.encode(), result.stdout+result.stderr)
        self.assertNotIn(READ_KEY.encode(), result.stdout+result.stderr)
        return result

    def test_both_adapters_isolate_grants_and_preserve_existing_connections_and_servers(self):
        for adapter in ("codex", "claude-code"):
            with self.subTest(adapter=adapter), tempfile.TemporaryDirectory(prefix="oojuro-combined-") as temporary:
                root = Path(temporary)
                command, env = self.fixture(root, adapter)
                result = self.run_setup(command, env, "--accept-local-capture")
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                report = json.loads(result.stdout.splitlines()[-1])
                self.assertTrue(report["capture_helper_completed"])
                self.assertTrue(report["project_helper_completed"])
                config = root/"agent-config.json"
                saved = config.read_bytes()
                self.assertEqual(json.loads(saved)["unrelated-server"], ["do-not-change"])
                capture, project = root/"capture/connection.json", root/"project/project.json"
                original = capture.read_bytes(), project.read_bytes()
                for path in (capture, project):
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertFalse((root/"capture/setup-api-key").exists())
                self.assertFalse((root/"project/setup-read-key").exists())
                for path in (root/"calls.jsonl", root/"capture/calls.jsonl", config):
                    self.assertNotIn(CAPTURE_KEY, path.read_text())
                    self.assertNotIn(READ_KEY, path.read_text())
                # Repeated registration refuses without replacing any stored grant/config.
                env.pop("OOJURO_API_KEY")
                env.pop("OOJURO_PROJECT_READ_KEY")
                result = self.run_setup(command, env, "--accept-local-capture")
                self.assertEqual(result.returncode, 2)
                self.assertEqual(json.loads(result.stdout.splitlines()[-1])["phase"], "project")
                self.assertEqual((capture.read_bytes(), project.read_bytes()), original)
                self.assertEqual(config.read_bytes(), saved)

    def test_preflight_does_not_read_keys_or_create_state(self):
        with tempfile.TemporaryDirectory(prefix="oojuro-combined-plan-") as temporary:
            root = Path(temporary)
            command, env = self.fixture(root)
            result = self.run_setup(command, env, "--dry-run")
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertFalse((root/"capture").exists())
            self.assertFalse((root/"project").exists())
            self.assertFalse((root/"calls.jsonl").exists())
            # Missing agent executable refuses during preflight, before capture writes.
            (root/"codex").unlink()
            env["PATH"] = str(root)
            result = self.run_setup(command, env, "--accept-local-capture")
            self.assertEqual(result.returncode, 2)
            self.assertEqual(json.loads(result.stdout.splitlines()[-1])["phase"], "preflight")
            self.assertFalse((root/"capture").exists())

    def test_capture_failure_stops_project_and_project_failure_reports_partial_setup(self):
        for phase in ("capture", "project"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory(prefix="oojuro-combined-partial-") as temporary:
                root = Path(temporary)
                command, env = self.fixture(root)
                config = root/"agent-config.json"
                saved = config.read_bytes()
                # The CLI is copied into the capture directory by the real helper.
                marker = root/"capture/capture-fails" if phase=="capture" else root/"project-fails"
                if phase=="capture":
                    marker.parent.mkdir(mode=0o700)
                marker.touch()
                result = self.run_setup(command, env, "--accept-local-capture")
                self.assertEqual(result.returncode, 2)
                report = json.loads(result.stdout.splitlines()[-1])
                self.assertEqual(report["phase"], phase)
                self.assertEqual(report["capture_helper_completed"], phase=="project")
                self.assertFalse(report["project_helper_completed"])
                self.assertEqual(config.read_bytes(), saved)
                self.assertEqual((root/"project").exists(), phase=="project")
                self.assertTrue((root/"capture/connection.json").exists())


if __name__ == "__main__":
    unittest.main()
