# Verified HTTPS transport fixture — 24 October 2026

Source: this capture-transport proposal, Node v24.22.1. The actual SDK sends
identical logical inputs in identity and explicit auto mode, alternating order,
three warmups and 12 measured samples per mode/case. The local server verifies TLS
with a synthetic localhost certificate and verifies every exact original before
its in-memory ACK. There were 240 complete acknowledgments and two TLS
connections, reused across requests. Body sizes exclude HTTP/TLS framing.

| Synthetic payload | Text bytes | Identity body | Auto body | Identity p50 ms | Auto p50 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| repeated-tool-output | 130 | 627 | 627 | 1.036 | 1.025 |
| repeated-tool-output | 8,352 | 8,851 | 310 | 1.047 | 2.022 |
| repeated-tool-output | 133,642 | 134,145 | 699 | 2.867 | 3.600 |
| repeated-tool-output | 1,069,136 | 1,069,641 | 3,422 | 12.140 | 13.644 |
| random-printable | 128 | 621 | 621 | 0.819 | 0.804 |
| random-printable | 8,192 | 8,687 | 6,461 | 0.884 | 1.398 |
| random-printable | 131,072 | 131,571 | 99,007 | 1.964 | 7.621 |
| random-printable | 1,048,576 | 1,049,077 | 790,060 | 17.017 | 66.056 |

Repeated tool output saves substantial bandwidth, but compression adds latency
on this fast loopback link. Random printable output is particularly expensive
with the platform's gzip encoder. Small requests use identity in both modes.
Identity therefore remains the default. These measurements do not establish a
Cloud ingest-speed improvement; opt-in compression targets bandwidth savings.
Do not infer network transmission time or WAN gains from the size ratio alone.

An untrusted-certificate negative control refuses the HTTPS connection before
any completed benchmark. The host also ran local Rust verification; timing is
indicative. The fixture has no disk commit, PostgreSQL, S3, KMS, tenant concurrency or WAN
latency. Tail percentiles with 12 samples are descriptive. The full results,
including adverse timings, are in [https-loopback.json](https-loopback.json).
The source and qualification hashes are recorded by the parent transport receipt.
See [reproduction instructions](../../CAPTURE_TRANSPORT.md).
