#!/usr/bin/env python3
"""Synthetic installer boundary tests; no actual agents, keys, network or user services."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("connect-agent.py")


class Installer(unittest.TestCase):
    def test_api_key_setup_has_no_browser_or_secret_arguments_and_preserves_restart(self):
        with tempfile.TemporaryDirectory(prefix="oojuro-installer-") as temporary:
            root = Path(temporary)
            transcript = root / "transcripts"
            transcript.mkdir(mode=0o700)
            binary = root / "oojuro"
            binary.write_text('''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
assert "OOJURO_API_KEY" not in os.environ
args=sys.argv[1:];command=args[args.index("cloud")+1]
with open(Path(__file__).with_name("calls.jsonl"),"a") as log:log.write(json.dumps(args)+"\\n")
path=Path(args[args.index("--connection")+1])
if command=="connect-api":
 key=Path(args[args.index("--api-key-file")+1]).read_text()
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
 with os.fdopen(fd,"w") as out:json.dump({"version":2,"integration_key":key},out)
 result={"journal_mode":"session","local_key_saved":True}
elif command=="capture-status":result={"journal":"a"*64,"journal_mode":"session"}
elif command=="capture":result={"delivered":0,"pending":0,"tracked_files":1,"new_lines":0}
else:raise Exception("unexpected command")
print(json.dumps(result))
''')
            binary.chmod(0o700)
            directory = root / "install"
            key = "synthetic-private-integration-key-never-log"
            env = dict(os.environ, OOJURO_API_KEY=key)
            command = [sys.executable, str(SCRIPT), "--binary", str(binary), "--adapter", "codex",
                       "--directory", str(directory), "--source-root", str(transcript),
                       "--name", "Code reviewer", "--accept-local-capture", "--no-service"]
            first = subprocess.run(command, env=env, capture_output=True, timeout=10)
            self.assertEqual(first.returncode, 0, first.stderr.decode())
            self.assertNotIn(key.encode(), first.stdout + first.stderr)
            self.assertNotIn(b"browser_approval", first.stdout)
            report = json.loads(first.stdout.splitlines()[-1])
            self.assertEqual(report["journal_mode"], "session")
            self.assertEqual(report["capture_status"], "waiting for new transcript activity")
            self.assertFalse(report["real_history_verified"])
            connection = directory / "connection.json"
            original = connection.read_bytes()
            self.assertEqual(connection.stat().st_mode & 0o777, 0o600)
            self.assertFalse((directory / "setup-api-key").exists())
            calls = (directory / "calls.jsonl").read_text()
            self.assertNotIn(key, calls)
            self.assertEqual(sum(json.loads(line)[json.loads(line).index("cloud")+1]=="connect-api" for line in calls.splitlines()), 1)
            env.pop("OOJURO_API_KEY")
            second = subprocess.run(command, env=env, capture_output=True, timeout=10)
            self.assertEqual(second.returncode, 0, second.stderr.decode())
            self.assertEqual(connection.read_bytes(), original)
            self.assertNotIn(key.encode(), second.stdout + second.stderr)

    def test_dry_run_needs_no_secret_or_installation_effects(self):
        with tempfile.TemporaryDirectory(prefix="oojuro-installer-plan-") as temporary:
            root = Path(temporary)
            transcript = root / "transcripts"
            transcript.mkdir(mode=0o700)
            binary = root / "oojuro"
            binary.write_text("synthetic binary, never executed")
            binary.chmod(0o700)
            directory = root / "install"
            result = subprocess.run([sys.executable, str(SCRIPT), "--binary", str(binary),
                "--adapter", "claude-code", "--directory", str(directory),
                "--source-root", str(transcript), "--dry-run"], capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(json.loads(result.stdout)["plan"]["journal"], "new journal per provider session")
            self.assertFalse(directory.exists())


if __name__ == "__main__":
    unittest.main()
