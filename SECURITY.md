# Security

This project is a pre-release embedded database engine. Applications own their
capture, recording and redaction policies. Local journal files, recordings and credentials belong to the
operator; do not attach them to public issues, pull requests, test fixtures or benchmark reports.

Report a vulnerability through this repository's **Security → Report a vulnerability** private
reporting flow. Include the affected commit and a minimal synthetic reproduction. Do not post
live credentials, account identifiers, private transcripts or an exploit against another person.
If private reporting is unavailable, contact the maintainer through their GitHub profile to
arrange a private channel; do not put the vulnerability details in a public issue.

There is no supported stable release yet. Maintainers triage against the current main branch;
published binaries require the separate native release verification and provenance process.
The engine stores caller-supplied records; it does not infer application recording policy.
Restrict database access at the operating-system boundary. Never submit real account data to tests.
