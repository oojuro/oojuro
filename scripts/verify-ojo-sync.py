#!/usr/bin/env python3
"""Linux-only syscall/fault qualification of public commit and archive syncs.

Uses a supplied io_profile binary, synthetic bytes and strace. No engine imports.
The archive's final data-sync EIO must prevent publication and preserve source.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess
import zlib


def source_records(path, expected=4):
    data = path.read_bytes()
    assert data[:8] == b'OJOFILE\0' and len(data) >= 64
    assert zlib.crc32(data[:60]) == struct.unpack_from('<I',data,60)[0]
    at, sequence, previous, records = 64, 0, bytes(32), []
    while at < len(data):
        head = data[at:at+96]
        assert len(head) == 96 and head[:8] == b'OJOTXN1\0'
        seq, stored, raw, count = struct.unpack_from('<QQQI',head,8)
        assert seq == sequence+1 and head[40:72] == previous and head[36] == 0
        assert zlib.crc32(head[:92]) == struct.unpack_from('<I',head,92)[0]
        body = data[at+96:at+96+stored]
        footer = data[at+96+stored:at+96+stored+48]
        digest = hashlib.sha256(data[:64]+head+body).digest()
        assert len(body) == raw and count == 1
        assert footer == b'OJOCMT1\0'+struct.pack('<Q',seq)+digest
        kind, reserved, key_len, value_len = struct.unpack_from('<B3sIQ',body)
        assert kind == 0 and reserved == bytes(3) and key_len == 8 and value_len == 128
        key, value = body[16:24], body[24:]
        ordinal = int.from_bytes(key,'big')
        text = b'synthetic trace: source bytes with exact line endings\r\n'
        original = bytearray((text*((128+len(text)-1)//len(text)))[:128])
        original[:8] = ordinal.to_bytes(8,'little')
        assert value == original
        records.append(ordinal)
        sequence, previous = seq, digest
        at += 96+stored+48
    assert records == list(range(expected)) and at == len(data)
    return hashlib.sha256(data).hexdigest()


def run(binary, output, label, mode, injection=None):
    trace, destination = output/(label+'.strace'), output/label
    command = ['strace','-qq','-yy','-s','64','-o',str(trace),
               '-e','trace=write,fsync,fdatasync,link,linkat,unlink,unlinkat']
    if injection:
        command += ['-e',f'inject=fsync:error=EIO:when={injection}']
    command += [str(binary),mode,str(destination),'4','1','128','100']
    result = subprocess.run(command,capture_output=True,text=True,timeout=60)
    lines = trace.read_text().splitlines()
    return result, lines, destination


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.output.mkdir(mode=0o700)
    summaries = []
    for mode in ['raw','archive']:
        result, lines, destination = run(binary,args.output,'positive-'+mode,mode)
        assert result.returncode == 0,result.stderr
        assert source_records(destination/'data.ojo')
        source_syncs, dirty = 0, False
        for line in lines:
            if '/data.ojo>' not in line:
                continue
            if line.startswith('write(') and re.search(r'"(?:OJOFILE|OJOTXN1)',line):
                assert not dirty,'next public transaction written before previous file sync'
                dirty = True
            if line.startswith('fsync('):
                assert line.endswith('= 0'),line
                source_syncs += 1
                dirty = False
        assert source_syncs == 5 and not dirty,(source_syncs,lines)
        summary = dict(mode=mode,public_file_syncs=source_syncs,source_records_verified=4)
        if mode == 'archive':
            publications = [i for i,line in enumerate(lines) if re.match(r'link(?:at)?\(',line) and 'archive.ojo' in line]
            assert len(publications) == 1
            publication = publications[0]
            archive_syncs = [i for i,line in enumerate(lines[:publication]) if line.startswith('fsync(') and '.tmp>' in line]
            assert len(archive_syncs) == 2,archive_syncs
            final_sync = archive_syncs[-1]
            # Include the directory syncs when locating the syscall to fault.
            ordinal = sum(line.startswith('fsync(') for line in lines[:final_sync+1])
            failed, fault_lines, fault_dir = run(binary,args.output,'archive-sync-failure','archive',ordinal)
            assert failed.returncode != 0,'injected final sync failure was acknowledged'
            assert any('INJECTED' in line and 'EIO' in line for line in fault_lines)
            assert not (fault_dir/'archive.ojo').exists(),'archive published before successful data sync'
            assert sorted(p.name for p in fault_dir.iterdir()) == ['data.ojo'],'unpublished temporary left behind'
            source_records(fault_dir/'data.ojo')
            summary.update(archive_file_syncs=2,final_sync_before_link=True,
                           injected_final_sync_ordinal=ordinal,final_sync_EIO_prevents_publication=True,
                           failed_archive_source_records_verified=4)
        summary['trace_sha256'] = hashlib.sha256(('\n'.join(lines)+'\n').encode()).hexdigest()
        summaries.append(summary)
    report = dict(scope='Linux local synthetic syscall qualification; not a power-cut test',checks=summaries)
    (args.output/'summary.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report,indent=2))


if __name__ == '__main__':
    main()
