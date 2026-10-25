#!/usr/bin/env python3
"""Independent, standard-library-only reader for raw plaintext Event profile 1.

Uses spec/0.1/FORMAT.md and spec/events/1/FORMAT.md. Explicitly refuses keys,
compression, >10,000 events and >64 MiB original bytes. Emits JSON hex so arbitrary
binary data round-trips. The reference Rust engine is neither imported nor run.
"""
import argparse
import fcntl
import hashlib
import json
import struct
import zlib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def checksum(header):
    return header[-4:] == struct.pack("<I", zlib.crc32(header[:-4]))


def read_events(path):
    events, requests = [], set()
    original_bytes, journal_id = 0, None
    with open(path, "rb") as stream:
        fcntl.flock(stream, fcntl.LOCK_SH | fcntl.LOCK_NB)
        file_header = stream.read(64)
        require(len(file_header) == 64 and file_header[:8] == b"OJOFILE\0"
                and checksum(file_header), "invalid file header")
        require(file_header[8:12] == b"\0\0\1\0" and file_header[40:60] == bytes(20),
                "unsupported wire version/reserved fields")
        require(file_header[12:16] == bytes(4) and file_header[32:40] == bytes(8),
                "this example supports plaintext files only")
        previous, sequence, tail = bytes(32), 1, 0
        while True:
            header = stream.read(96)
            if not header:
                break
            if len(header) < 96:
                tail = len(header)
                break
            require(header[:8] == b"OJOTXN1\0" and checksum(header), "invalid transaction header")
            actual_sequence, stored, decoded, operations = struct.unpack_from("<QQQI", header, 8)
            require(actual_sequence == sequence and header[40:72] == previous,
                    "transaction order or digest chain")
            require(header[36] in (0, 1) and header[37:40] == bytes(3)
                    and header[88:92] == bytes(4), "unsupported codec/reserved fields")
            nonce = header[72:88]
            require(nonce != bytes(16) and nonce not in requests, "invalid/duplicate request nonce")
            require(stored <= 33 << 20 and decoded <= 32 << 20 and 1 <= operations <= 1024,
                    "wire resource bounds")
            raw = stream.read(stored + 48)
            if len(raw) < stored + 48:
                tail = 96 + len(raw)
                break
            body, footer = raw[:stored], raw[stored:]
            digest = hashlib.sha256(file_header + header + body).digest()
            require(footer[:8] == b"OJOCMT1\0" and footer[8:16] == header[8:16]
                    and footer[16:] == digest, "invalid commit footer/digest")
            require(header[36] == 0, "this example supports raw codec only")
            require(decoded == len(body) and operations == 1 and len(body) >= 16,
                    "invalid Event transaction shape")
            kind, reserved, key_length, value_length = struct.unpack_from("<B3sIQ", body)
            require(kind == 0 and reserved == bytes(3) and 1 <= key_length <= 65536
                    and value_length <= 16 << 20 and len(body) == 16 + key_length + value_length,
                    "invalid Event operation grammar")
            key, value = body[16:16 + key_length], body[16 + key_length:]
            if sequence == 1:
                require(key == b"\0ojo.events/1" and len(value) == 24
                        and value[:8] == b"OJOEVT1\0" and value[8:] != bytes(16),
                        "Event profile 1 required")
                journal_id = value[8:]
            else:
                require(len(key) == 36 and key[:4] == b"\0evt"
                        and struct.unpack_from(">Q", key, 4)[0] == sequence - 1
                        and key[20:] == nonce, "invalid Event key/ordinal/nonce")
                original_bytes += len(value)
                require(len(events) < 10000 and original_bytes <= 64 << 20,
                        "example reader resource budget exceeded")
                cursor = journal_id + key[4:12] + nonce
                events.append({"cursor_hex": cursor.hex(),
                               "accepted_at": struct.unpack_from(">q", key, 12)[0],
                               "data_hex": value.hex()})
            requests.add(nonce)
            previous, sequence = digest, sequence + 1
        require(journal_id is not None, "complete Event profile marker required")
    return {"profile": 1, "journal_id_hex": journal_id.hex(), "events": events,
            "incomplete_tail_bytes": tail}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path")
    args = parser.parse_args()
    try:
        result = read_events(args.path)
    except (ValueError, OSError) as error:
        parser.exit(2, f"read_events: {error}\n")
    print(json.dumps(result, ensure_ascii=True))
