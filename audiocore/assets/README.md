# Vendored model weights

## `silero_vad_16k_op15.onnx`

Voice-activity detection at ingest (stage D4). Embedded into the `recalld`
binary with `include_bytes!` (see `src/vad.rs`) rather than read from a path:
the deployment is a container plus a k8s secret, and a model loaded from disk is
one more thing a rollout can forget while everything still starts. Embedding
makes "the binary exists" and "the model exists" the same fact.

- **Source:** the `silero-vad` PyPI package, version **6.2.1**, file
  `silero_vad/data/silero_vad_16k_op15.onnx`, taken from this repo's own nix
  dev-env rather than downloaded ad hoc.
- **sha256:** `7ed98ddbad84ccac4cd0aeb3099049280713df825c610a8ed34543318f1b2c49`
- **Licence:** MIT (silero-vad, github.com/snakers4/silero-vad).
- **Why this variant:** 16 kHz-only, opset 15. Every segment reaching the
  detector is already decoded to 16 kHz mono (ASR's shape, and what the room
  builder emits), so the multi-rate model's extra input and size buy nothing.

⚠ **The Python pipeline does NOT use this file.** `recall.vad` loads silero
through torch as a JIT model (`silero_vad.jit`); the ONNX files merely ship in
the same wheel. So the two implementations share weights and an origin, but not
a runtime — and agreement between them is something to MEASURE, not assume.
Their pre-processing must match deliberately: `recall.vad` lifts a clip's peak
toward 0.5 with gain capped at 32x before detection, which is what stopped quiet
phone mics from being gated to silence. A Rust port that skips that gain will
disagree with Python exactly where it matters most.

To re-derive, from the devshell:

    python -c "import silero_vad, pathlib; print(pathlib.Path(silero_vad.__file__).parent / 'data')"
