#!/usr/bin/env python3
"""Fixed AB/BA warm serial Record API diagnostic using explicit frozen binaries."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--control', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--profile-source', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--records', type=int, default=100000)
    parser.add_argument('--searches', type=int, default=200)
    args = parser.parse_args()
    if not 1000 <= args.records <= 100000 or not 10 <= args.searches <= 1000:
        parser.error('1k..100k records and 10..1000 lexical search samples required')
    os.umask(0o077)
    args.out.mkdir(mode=0o700)
    lock = Path.home()/'.cache/oojuro-bench.lock'
    lock.parent.mkdir(exist_ok=True)
    runs = []
    with lock.open('a') as held:
        fcntl.flock(held, fcntl.LOCK_EX)
        for index, label in enumerate(['control', 'candidate', 'candidate', 'control']):
            binary = args.control if label == 'control' else args.candidate
            name = f'{index}-{label}'
            print('public API run '+name+' started', flush=True)
            load = os.getloadavg()
            with (args.out/(name+'.stderr')).open('xb') as error:
                result = subprocess.run([str(binary), str(args.out/name), str(args.records), name,
                    str(args.searches)], stdout=subprocess.PIPE, stderr=error,
                    timeout=300, start_new_session=True)
            (args.out/(name+'.stdout')).write_bytes(result.stdout)
            assert result.returncode == 0, (name, result.returncode)
            response = json.loads(result.stdout)
            response.update(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                load_average_before=load, load_average_after=os.getloadavg())
            runs.append(response)
            (args.out/(name+'.json')).write_text(json.dumps(response, indent=2)+'\n')
            print(json.dumps(response), flush=True)
    assert len({r['ranked_signature'] for r in runs}) == 1
    assert len({r['original_digest'] for r in runs}) == 1
    assert all(r['originals_verified'] == args.records for r in runs)
    assert runs[0]['binary_sha256'] != runs[1]['binary_sha256']
    (args.out/'summary.json').write_text(json.dumps({
        'scope': 'Public Record API synthetic warm serial development comparison; no cloud, 1M/concurrency or SLA acceptance',
        'identical_profile_source_sha256': hashlib.sha256(args.profile_source.read_bytes()).hexdigest(),
        'fixed_order': ['control', 'candidate', 'candidate', 'control'],
        'per_arm_build_targets_isolated': True,
        'cold_io_or_cgroup_isolation': False,
        'tail_sample_warning': '1,000 samples per read width per run; 200 default unique-term searches; not the owner mixed-query scale campaign',
        'runs': runs}, indent=2)+'\n')


if __name__ == '__main__':
    main()
