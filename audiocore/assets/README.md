# Vendored model weights

## `silero_vad_16k_op15.onnx`

The speech detector, embedded with `include_bytes!` (`src/vad.rs`) so no
deployment can lack it. Every speech measurement, fleet and Mac, goes through it.

- Source: the `silero-vad` PyPI package 6.2.1, `silero_vad/data/`, from this
  repo's dev-env. MIT.
- sha256: `7ed98ddbad84ccac4cd0aeb3099049280713df825c610a8ed34543318f1b2c49`
- 16 kHz only, opset 15: segments reach it already decoded to 16 kHz mono.

To find it in the devshell:

    python -c "import silero_vad, pathlib; print(pathlib.Path(silero_vad.__file__).parent / 'data')"
