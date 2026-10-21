"""Shared helpers for the activity-search prototypes (docs/ACTIVITY_SEARCH.md).

Run every script with `python3 -I` (isolated: no user site, no PYTHON* env).
Input is `oojuro export` JSON lines: a path (plain or `.zst`) or `-` for
stdin. Everything streams: one record in memory at a time, plus bounded
per-writer state.

Nothing here reads text into any output: callers write numbers only.
"""
import json
import re
import sys
from compression import zstd

KINDS = ["system", "user_input", "thinking", "assistant", "tool_call", "tool_result", "explicit"]
KIND = {k: i for i, k in enumerate(KINDS)}

# Tool families: what kind of work an event is (rules over tool names and
# shell programs; no model). `user`, `say`, `think` are the non-tool kinds.
FAMILIES = ["read", "edit", "run", "test", "vcs", "wait", "delegate", "ask", "web", "other",
            "user", "say", "think", "note"]
FAM = {f: i for i, f in enumerate(FAMILIES)}


def open_records(path):
    """Yield export records (dicts) from a path, a `.zst` path, or `-`."""
    if path == "-":
        f = sys.stdin
    elif path.endswith(".zst"):
        f = zstd.open(path, "rt", encoding="utf-8")
    else:
        f = open(path, encoding="utf-8")
    with f:
        for line in f:
            if line.startswith("{"):
                yield json.loads(line)


def meta(r):
    m = r.get("meta")
    return m if isinstance(m, dict) else {}


def is_echo(r):
    """Oojuro's own echo (`oojuro=call|result|context`): stored, never activity."""
    return "oojuro" in meta(r)


def is_activity(r):
    """Events that count as activity: everything but harness context and echo."""
    return r["kind"] != "system" and not is_echo(r)


# ------------------------------------------------------------------ errors

_HARD = re.compile(
    r"^(?:Exit code [1-9]\d*|<tool_use_error>|\S+ hook error:|\[Request interrupted|"
    r"Script failed|Script error:|Internal Error|Failed to create stream fd|"
    r"Error: |error: |Command timed out)"
)
_SOFT = re.compile(
    r"Traceback \(most recent call last\)|^error(?:\[E\d+\])?: |panicked at |"
    r"^\s*FAIL(?:ED)?\b|\bFailed Tests\b|test result: FAILED|npm ERR!|^fatal: |"
    r"command not found|No such file or directory|Permission denied|"
    r"\b[1-9]\d* (?:failed|failing|errors?)\b|^Error\b|Uncaught |Unhandled ",
    re.M,
)
ERROR_SUBS = {"api_error", "upstream_error"}


def error_level(r):
    """0 = none, 1 = error output (the text shows a failure), 2 = failed call
    (the harness reports a failure, a block, an interrupt, an API error).

    The engine reads the stored status first (`is_error` for Claude Code);
    `oojuro export` does not carry it, so the prototype infers it from the
    text, which is also what formats without a status (Codex) need."""
    k = r["kind"]
    if k == "system":
        return 2 if meta(r).get("sub") in ERROR_SUBS else 0
    if k != "tool_result" or is_echo(r):
        return 0
    t = r["text"]
    if _HARD.match(t):
        return 2
    head, tail = t[:2048], t[-2048:] if len(t) > 2048 else ""
    if _SOFT.search(head) or (tail and _SOFT.search(tail)):
        return 1
    return 0


# ------------------------------------------------------------------ families

_READ_PROGS = set(
    "cat head tail grep rg egrep fgrep ls find fd wc jq yq awk tree stat file less more diff du df ps pgrep "
    "ss lsof which whereis date env printenv readlink realpath sort uniq cut column xxd od hexdump basename "
    "dirname echo printf true false test [ type nl strings sha256sum md5sum cmp free uptime id whoami "
    "hostname uname nproc top htop journalctl systemctl".split()
)
_EDIT_PROGS = set("rm mv cp mkdir rmdir touch chmod chown ln tee patch truncate install unzip tar".split())
_WAIT_PROGS = set("sleep until while wait".split())
_VCS_WRITE = set(
    "commit push merge rebase checkout switch worktree reset cherry-pick stash tag fetch pull clone am "
    "apply restore revert init add rm mv".split()
)
_WRAPPERS = set("env exec command nohup time sudo nice timeout ionice stdbuf xargs".split())
_SKIP = set("cd pushd popd export set unset source . local declare readonly alias shopt trap :".split())
_TESTISH = re.compile(r"(?:^|[\s/])(?:test|tests|pytest|vitest|jest|playwright|nextest|check|clippy|lint|tsc)\b")


def _simple_commands(cmd):
    cmd = cmd.replace("$(", "\n")
    return [c for c in re.split(r"&&|\|\||[;|\n(){}`]", cmd) if c.strip()]


def _program(words):
    i = 0
    while i < len(words):
        w = words[i].strip("\"'")
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", w):
            i += 1
            continue
        if w in _WRAPPERS:
            i += 1
            while i < len(words) and (words[i].startswith("-") or re.match(r"^[\d.]+[smhd]?$", words[i])):
                i += 1
            continue
        return w.rsplit("/", 1)[-1], words[i + 1:]
    return "", []


def shell_family(cmd):
    """Family of a shell command line: the strongest of its simple commands
    (test > run > vcs > edit > wait > read)."""
    best = None
    rank = {"test": 6, "run": 5, "vcs": 4, "edit": 3, "wait": 2, "read": 1}
    for sc in _simple_commands(cmd):
        words = sc.split()
        if not words:
            continue
        prog, rest = _program(words)
        if not prog or prog in _SKIP:
            continue
        if prog in ("for", "if", "do", "then", "else", "fi", "done", "case", "esac"):
            continue
        if prog in _WAIT_PROGS:
            fam = "wait"
        elif prog == "git":
            sub = next((w for w in rest if not w.startswith("-")), "")
            fam = "vcs" if sub in _VCS_WRITE else "read"
        elif prog == "gh":
            fam = "wait" if "watch" in rest else ("vcs" if rest[:1] in (["pr"], ["release"]) else "run")
        elif prog == "sed":
            fam = "edit" if any(w.startswith("-i") for w in rest) else "read"
        elif prog in _READ_PROGS:
            fam = "read"
            if prog == "cat" and ">" in sc:
                fam = "edit"
        elif prog in _EDIT_PROGS:
            fam = "edit"
        elif _TESTISH.search(" " + " ".join([prog] + rest[:6])):
            fam = "test"
        else:
            fam = "run"
        if best is None or rank[fam] > rank[best]:
            best = fam
    return best or "read"


_MCP_READ = re.compile(r"__(?:get|list|search|read|query|fetch|find|describe|show|resolve|view)[_a-z]*$")
_CODEX_CMD = re.compile(r"""\bcmd\s*:\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`([^`]*)`)""")
_TOOL_FAM = {
    "Read": "read", "Grep": "read", "Glob": "read", "LS": "read", "NotebookRead": "read", "ToolSearch": "read",
    "Skill": "read", "view_image": "read",
    "WebFetch": "web", "WebSearch": "web",
    "Write": "edit", "Edit": "edit", "MultiEdit": "edit", "NotebookEdit": "edit", "apply_patch": "edit",
    "Agent": "delegate", "Task": "delegate", "SendMessage": "delegate", "send_message": "delegate",
    "followup_task": "delegate", "list_agents": "delegate", "TaskStop": "delegate", "spawn_agent": "delegate",
    "AskUserQuestion": "ask", "request_user_input": "ask", "request_user_input_async": "ask",
    "Monitor": "wait", "wait_agent": "wait", "TaskOutput": "wait", "sleep": "wait",
}


def call_family(tool, text):
    """Family of a tool call from its tool name and input text."""
    if tool in ("Bash", "bash", "shell", "exec_command", "local_shell"):
        try:
            v = json.loads(text)
            cmd = (v.get("command") or v.get("cmd") or "") if isinstance(v, dict) else text
            if isinstance(cmd, list):
                cmd = " ".join(map(str, cmd))
        except ValueError:
            cmd = text
        return shell_family(str(cmd))
    if tool == "exec":  # Codex code mode: a script calling tools.*
        if "tools.apply_patch" in text:
            return "edit"
        cmds = ["".join(m) for m in _CODEX_CMD.findall(text)]
        if cmds:
            fams = [shell_family(c.encode().decode("unicode_escape", "ignore")) for c in cmds]
            for f in ("test", "run", "vcs", "edit", "wait", "read"):
                if f in fams:
                    return f
        if "tools.web__" in text:
            return "web"
        if "tools.write_stdin" in text:
            return "wait"
        return "run"
    if tool in _TOOL_FAM:
        return _TOOL_FAM[tool]
    if tool.startswith("mcp__claude-in-chrome") or "browser" in tool.lower():
        return "web"
    if tool.startswith("mcp__") or "__" in tool:
        return "read" if _MCP_READ.search(tool) else "run"
    return "other"


_PATCH_FILE = re.compile(r"^\*\*\* (?:Add|Update|Delete) File: (\S+)", re.M)


def call_paths(tool, text):
    """File paths a tool call names (structured fields, patch headers)."""
    out = []
    if text.startswith("{"):
        try:
            v = json.loads(text)
        except ValueError:
            v = None
        if isinstance(v, dict):
            for k in ("file_path", "path", "notebook_path"):
                p = v.get(k)
                if isinstance(p, str) and p:
                    out.append(p)
    if "*** " in text:
        out.extend(_PATCH_FILE.findall(text))
    return out


class Families:
    """Per session: the family of each open call, so a result gets its
    call's family (FIFO per tool name; the export has no parent link)."""

    def __init__(self):
        self.pending = {}

    def of(self, r):
        k = r["kind"]
        if k == "user_input":
            return "user"
        if k == "assistant":
            return "say"
        if k == "thinking":
            return "think"
        if k == "explicit":
            return "note"
        tool = r.get("tool", "")
        key = (r["session"], tool)
        if k == "tool_call":
            fam = call_family(tool, r["text"])
            q = self.pending.setdefault(key, [])
            q.append(fam)
            if len(q) > 64:
                del q[0]
            return fam
        if k == "tool_result":
            q = self.pending.get(key)
            if q:
                fam = q.pop(0)
                if not q:
                    del self.pending[key]
                return fam
            return call_family(tool, "")
        return "other"


# ------------------------------------------------------------------ CDC

def _gear():
    # deterministic 32-bit gear table (splitmix-style)
    x = 0x9E3779B97F4A7C15
    g = []
    for _ in range(256):
        x = (x + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        z = x
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        g.append((z ^ (z >> 31)) & 0xFFFFFFFF)
    return g


GEAR = _gear()
CDC_MIN, CDC_AVG, CDC_MAX = 256, 1024, 8192
CDC_MASK = 0xFFC00000  # 10 high bits: one boundary per ~1 KiB past the minimum


def cdc_chunks(data):
    """Content-defined chunks (gear hash, 256 B / ~1 KiB / 8 KiB), like the
    store's FastCDC dedupe in spirit (not byte-identical boundaries)."""
    n = len(data)
    out = []
    start = 0
    g = GEAR
    mask = CDC_MASK
    while start < n:
        end = min(n, start + CDC_MAX)
        if end - start <= CDC_MIN:
            out.append((start, end))
            break
        h = 0
        i = start + CDC_MIN
        cut = end
        for b in data[i:end]:
            h = ((h << 1) + g[b]) & 0xFFFFFFFF
            i += 1
            if not h & mask:
                cut = i
                break
        out.append((start, cut))
        start = cut
    return out
