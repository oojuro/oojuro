#!/usr/bin/env python3
"""Generate synthetic plaintext Event vectors independently of the Rust engine."""
import hashlib
import json
from pathlib import Path
import struct
import zlib


def seal(header):
    header[-4:] = struct.pack("<I", zlib.crc32(header[:-4]))
    return bytes(header)


def transaction(file_header, sequence, previous, nonce, key, value):
    body = struct.pack("<B3sIQ", 0, bytes(3), len(key), len(value)) + key + value
    header = bytearray(96)
    header[:8] = b"OJOTXN1\0"
    struct.pack_into("<QQQI", header, 8, sequence, len(body), len(body), 1)
    header[40:72], header[72:88] = previous, nonce
    header = seal(header)
    digest = hashlib.sha256(file_header + header + body).digest()
    return header + body + b"OJOCMT1\0" + struct.pack("<Q", sequence) + digest, digest


def vector(name, data):
    header = bytearray(64)
    header[:8] = b"OJOFILE\0"
    struct.pack_into("<H", header, 10, 1)
    header[16:32] = bytes.fromhex("10" * 16)
    header = seal(header)
    journal_id = bytes.fromhex("20" * 16)
    marker, previous = transaction(header, 1, bytes(32), bytes.fromhex("30" * 16),
                                   b"\0ojo.events/1", b"OJOEVT1\0" + journal_id)
    raw, expected = header + marker, []
    for ordinal, (timestamp, value) in enumerate(data, 1):
        nonce = ordinal.to_bytes(16, "big")
        key = b"\0evt" + struct.pack(">Qq", ordinal, timestamp) + nonce
        frame, previous = transaction(header, ordinal + 1, previous, nonce, key, value)
        raw += frame
        expected.append({"cursor_hex": (journal_id + key[4:12] + nonce).hex(),
                         "accepted_at": timestamp, "data_hex": value.hex()})
    return {"name": name, "file_hex": raw.hex(), "events": expected}


def vectors():
    return {"profile": "1.0.0-draft.1", "wire": "0.1",
            "generator": "Independent Python stdlib encoder in generate_vectors.py",
            "valid": [vector("empty-profile", []),
                      vector("binary-repeated-and-backward-clock",
                             [(17, b"sensor=alpha\n"), (-3, b"\0\xff\r\n"), (17, b"")])]}


if __name__ == "__main__":
    target = Path(__file__).with_name("vectors.json")
    target.write_text(json.dumps(vectors(), indent=2) + "\n")
