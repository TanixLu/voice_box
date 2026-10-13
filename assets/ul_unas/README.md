# UL-UNAS DNS3

Official model and architecture:
[Xiaobin-Rong/ul-unas](https://github.com/Xiaobin-Rong/ul-unas/tree/00f7c700da43d38347f30a6ccebd86fcbc798e07).
The upstream MIT notice is retained in `LICENSE`.

- Checkpoint: `checkpoints/model_trained_on_dns3.tar`, epoch 191.
- Checkpoint SHA256: `9a0656bf8e88d36865792d3065fed9bc33d5ab85e11ae3a1ae085b928fdb49a8`.
- `dns3.f32`: 172,290 little-endian float32 values from the checkpoint's model
  state, including ERB matrices, normalization and affine PReLU parameters.
  Noncontiguous tensors are stored in logical row-major order.
- Tensor SHA256: `fadb250f98de9d9c703b912c2057f1dd685ad6b5ddd2a4dff1dace2c07116bbd`.
- Typed offsets and dimensions: `src/apo/dsp/ul_unas/weights.rs`.
- Native network: `src/apo/dsp/ul_unas/network.rs`, matching the official
  frame-wise `ulunas_onnx/stream/ulunas_stream.py` architecture.

UL-UNAS uses 16 kHz audio, a 512-point periodic Hann STFT, a 256-sample hop,
65 uncompressed plus 64 ERB feature bins, affine PReLU, causal time-frequency
attention and two grouped dual-path recurrent blocks. Its real-valued mask
scales both components of the noisy spectrum. The native STFT normalizes the
Hann-window overlap; its intrinsic delay is 256 samples (16 ms).

The model runs synchronously without allocating during inference or reset.
Weights are decoded with `f32::from_le_bytes` during compilation. All three
models are prepared when the audio stream opens; only the selected model runs.
Switching fades out, resets, fills buffers in silence and fades in at the same
device-frame delay as the other choices. No Python or inference runtime is
required by the application.
