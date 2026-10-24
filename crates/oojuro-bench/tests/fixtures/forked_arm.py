#!/usr/bin/env python3
"""Synthetic protocol arm with a child in a DIFFERENT session/process group."""
import json, os, subprocess, sys
child = subprocess.Popen(
    [sys.executable, '-c', 'import os,time; os.setsid(); time.sleep(300)'],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for line in sys.stdin:
    request = json.loads(line)
    if request['op'] == 'hello':
        answer = {'ok': True, 'info': {'name':'fork-fixture', 'version':'1',
            'commit':'synthetic', 'durability':'none', 'protocol':'oojuro-arm-v1', 'notes':[]}}
    elif request['op'] == 'stats':
        answer = {'ok': True, 'stats': {'child_pid': child.pid}}
    else:
        raise RuntimeError('unexpected fixture request')
    print(json.dumps(answer), flush=True)
