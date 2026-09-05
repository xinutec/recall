"""The device-ingest wire protocol — the few facts both ends must agree on.

Its own module, with **no imports**, because the two ends do not share a runtime.
The server is audiod (Rust — `audiod ingest`); a Linux mic client (`recall.mic`)
runs on a box that has a bare `python3` and no venv, and must not drag the store,
pydantic or the ML stack in behind a port number.

No client can import the server's parser, so the shared contract lives in
`audiod/tests/handshakes.json`: the canonical line each client emits. audiod's
`wire_fixture` test asserts the live parser accepts every one; `tests/test_mic.py`
asserts this client still produces its line byte for byte — the drift that would
otherwise go unnoticed until a mic silently stopped connecting.
"""

from __future__ import annotations

# The one shared port every device connects to — hard-coded, so nothing needs setting
# on a device but the host.
DEFAULT_INGEST_PORT = 9999

# The PCM the pipeline speaks end to end: 48 kHz signed 16-bit little-endian.
# `capture.build_segment_argv` reads exactly this off the wire.
SAMPLE_RATE = 48000
BYTES_PER_SAMPLE = 2
