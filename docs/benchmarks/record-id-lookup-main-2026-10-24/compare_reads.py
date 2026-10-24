#!/usr/bin/env python3
"""Bounded synthetic arm-protocol diagnostic; not a hosted/concurrent SLA test."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import select
import signal
import subprocess
import time


def doc(number):
    digest = hashlib.sha256(f'ojo-lookup-corpus-v1:{number}'.encode()).hexdigest()
    text = (f'agent journal synthetic tool result needle{number:08d} '
            f'path module{number % 29} verified original {digest} λ\n')
    return {'id': f'row{number:08d}', 'text': text, 'valid_at': number,
            'recorded_at': number, 'meta': {'fixture': 'ojo-lookup-v1'}}


def quantiles(samples):
    ordered = sorted(samples)
    return {'n': len(ordered), **{f'p{p}_us': ordered[max(0, math.ceil(len(ordered)*p/100)-1)]
                                 for p in [50, 95, 99]}}


def footprint(root):
    files = []
    for file in sorted(root.rglob('*')):
        if file.is_symlink():
            raise RuntimeError('unexpected fixture symlink')
        if file.is_file():
            info = file.stat()
            files.append({'path': str(file.relative_to(root)), 'bytes': info.st_size,
                          'allocated': info.st_blocks*512})
    return {'apparent_bytes': sum(x['bytes'] for x in files),
            'allocated_bytes': sum(x['allocated'] for x in files), 'files': files}


class Arm:
    def __init__(self, binary, arm, error_path):
        self.errors = error_path.open('xb')
        self.process = subprocess.Popen([str(binary), 'serve', '--arm', arm],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.errors,
            start_new_session=True)
        os.set_blocking(self.process.stdin.fileno(), False)
        os.set_blocking(self.process.stdout.fileno(), False)

    def rpc(self, request):
        start = time.monotonic_ns()
        pending = memoryview((json.dumps(request, separators=(',', ':'))+'\n').encode())
        data = bytearray()
        deadline = time.monotonic()+45
        while pending or not data.endswith(b'\n'):
            remaining = deadline-time.monotonic()
            if remaining <= 0:
                raise TimeoutError('arm request exceeded 45 seconds')
            readable, writable, _ = select.select([self.process.stdout],
                [self.process.stdin] if pending else [], [], remaining)
            if writable:
                pending = pending[os.write(self.process.stdin.fileno(), pending[:65536]):]
            if readable:
                chunk = os.read(self.process.stdout.fileno(), 1 << 20)
                if not chunk:
                    raise RuntimeError('arm exited before response')
                data.extend(chunk)
                if len(data) > 64 << 20:
                    raise RuntimeError('response exceeded diagnostic bound')
        response = json.loads(data)
        if not response.get('ok'):
            raise RuntimeError('arm refused request: '+str(response.get('error')))
        wall_us = (time.monotonic_ns()-start)/1000
        return response, wall_us

    def hwm(self):
        for line in Path(f'/proc/{self.process.pid}/status').read_text().splitlines():
            if line.startswith('VmHWM:'):
                return int(line.split()[1])*1024
        raise RuntimeError('process high-water RSS unavailable')

    def stop(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)
        self.errors.close()
        assert self.process.poll() == 0, 'arm did not exit cleanly'


def run(binary, arm_name, label, out, count, attempts, searches):
    directory = out/label
    directory.mkdir()
    store = directory/'store'
    arm = Arm(binary, arm_name, directory/'stderr.log')
    result = {'label': label, 'arm': arm_name, 'records': count,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'load_average_before': os.getloadavg(), 'errors': 0}
    try:
        result['arm_info'] = arm.rpc({'op': 'hello'})[0]['info']
        _, result['create_open_us'] = arm.rpc({'op': 'open', 'dir': str(store)})
        batch_times, single_times = [], []
        logical = 0
        corpus = hashlib.sha256()
        for at in range(0, count-33, 1000):
            rows = [doc(n) for n in range(at, min(at+1000, count-33))]
            response, wall = arm.rpc({'op': 'append_batch', 'docs': rows})
            batch_times.append((wall, response['elapsed_us']))
            for row in rows:
                logical += len(row['text'].encode())
                corpus.update(json.dumps(row, sort_keys=True, ensure_ascii=False).encode()+b'\n')
        for number in range(count-33, count-1):
            row = doc(number)
            response, wall = arm.rpc({'op': 'append_batch', 'docs': [row]})
            single_times.append((wall, response['elapsed_us']))
            logical += len(row['text'].encode())
            corpus.update(json.dumps(row, sort_keys=True, ensure_ascii=False).encode()+b'\n')
        result['append_batch_1000'] = {'wall': quantiles([x[0] for x in batch_times]),
                                     'engine': quantiles([x[1] for x in batch_times])}
        result['append_single'] = {'wall': quantiles([x[0] for x in single_times]),
                                  'engine': quantiles([x[1] for x in single_times])}
        reads = {1: [], 20: []}
        for iteration in range(attempts+32):
            width = 1 if iteration % 2 == 0 else 20
            numbers = [(iteration*104729+j*8191) % (count-1) for j in range(width)]
            response, wall = arm.rpc({'op': 'get', 'ids': [doc(n)['id'] for n in numbers]})
            assert response['texts'] == [doc(n)['text'] for n in numbers]
            if iteration >= 32:
                reads[width].append((wall, response['elapsed_us']))
        for width, series in reads.items():
            result[f'get_{width}'] = {'wall': quantiles([x[0] for x in series]),
                                    'engine': quantiles([x[1] for x in series])}
        timings = []
        ranked = hashlib.sha256()
        for iteration in range(searches+1):
            number = (iteration*104729) % (count-1)
            query = {'text': f'needle{number:08d}', 'valid_at': count,
                     'known_at': count, 'k': 20}
            response, wall = arm.rpc({'op': 'search', 'query': query})
            assert [x['id'] for x in response['hits']] == [doc(number)['id']], response['hits']
            if iteration:
                timings.append((wall, response['elapsed_us']))
                ranked.update(json.dumps(response['hits'], sort_keys=True).encode()+b'\n')
        result['search_unique'] = {'wall': quantiles([x[0] for x in timings]),
                                   'engine': quantiles([x[1] for x in timings]),
                                   'ranked_signature': ranked.hexdigest()}
        row = doc(count-1)
        arm.rpc({'op': 'append_batch', 'docs': [row]})
        logical += len(row['text'].encode())
        corpus.update(json.dumps(row, sort_keys=True, ensure_ascii=False).encode()+b'\n')
        query = {'text': f'needle{count-1:08d}', 'valid_at': count, 'known_at': count, 'k': 20}
        assert [x['id'] for x in arm.rpc({'op': 'search', 'query': query})[0]['hits']] == [row['id']]
        query['known_at'] = count-2
        assert not arm.rpc({'op': 'search', 'query': query})[0]['hits']
        result['stats_open'] = arm.rpc({'op': 'stats'})[0].get('stats')
        result['open_footprint'] = footprint(store)
        arm.rpc({'op': 'close'})
        result['closed_footprint'] = footprint(store)
        _, result['reopen_us'] = arm.rpc({'op': 'open', 'dir': str(store)})
        response, _ = arm.rpc({'op': 'get_range', 'from': 0, 'to': count, 'limit': count})
        assert len(response['docs']) == count
        for number, original in enumerate(response['docs']):
            assert original == doc(number), (number, original)
        result.update(acknowledged_originals_verified=count, logical_text_bytes=logical,
                      corpus_sha256=corpus.hexdigest(), high_water_rss_bytes=arm.hwm(),
                      freshness_and_past_clock_oracles=True,
                      load_average_after=os.getloadavg(), status='passed')
        arm.rpc({'op': 'close'})
    finally:
        arm.stop()
    (directory/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps({k: result[k] for k in ['label', 'arm', 'records', 'status', 'get_1', 'get_20', 'search_unique']}), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--control', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--records', type=int, default=10000)
    parser.add_argument('--reads', type=int, default=2000)
    parser.add_argument('--searches', type=int, default=50)
    parser.add_argument('--paired-rounds', type=int, default=2)
    args = parser.parse_args()
    assert 1000 <= args.records <= 100000 and args.reads >= 64 and args.searches >= 1
    args.out.mkdir(mode=0o700)
    lock = Path.home()/'.cache/oojuro-bench.lock'
    lock.parent.mkdir(exist_ok=True)
    with lock.open('a') as held:
        fcntl.flock(held, fcntl.LOCK_EX)
        results = []
        for number in range(args.paired_rounds):
            order = [('control', args.control), ('candidate', args.candidate)]
            if number % 2:
                order.reverse()
            for label, binary in order:
                results.append(run(binary, 'native-record', f'round{number}-{label}',
                    args.out, args.records, args.reads, args.searches))
        for arm in ['sqlite', 'jsonl', 'jsonl-zstd']:
            results.append(run(args.candidate, arm, arm, args.out, args.records, args.reads, args.searches))
        assert len({r['corpus_sha256'] for r in results}) == 1
        native = [r for r in results if r['arm'] == 'native-record']
        assert len({r['search_unique']['ranked_signature'] for r in native}) == 1
        (args.out/'summary.json').write_text(json.dumps({'scope': 'Local warm serial synthetic development diagnostic; no cloud, one-million, concurrent or SLA claim',
            'controls': 'Fixed arm order AB/BA, identical originals/queries, existing arm protocol; no fs cache drop or cgroup isolation',
            'limitations': ['Unique-token search is correctness/work diagnostic, not representative relevance or query mix',
                            'Short search/write series do not qualify tail percentiles or general throughput gains',
                            'Native local encryption differs from plaintext SQLite/JSONL; not matched confidentiality or hosted failure domains',
                            'Seed/reopen/full original verification included in each fresh store; clean reopen only, no new crash campaign'],
            'runs': results}, indent=2)+'\n')


if __name__ == '__main__':
    main()
