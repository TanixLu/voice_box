# DeepFilterNet3

Official standard DeepFilterNet3 model from
[Rikorose/DeepFilterNet](https://github.com/Rikorose/DeepFilterNet/tree/d375b2d8309e0935d165700c91da9de862a99c31).
Upstream MIT and Apache-2.0 notices are retained alongside the model.

- Source archive: [`models/DeepFilterNet3_onnx.tar.gz`](https://raw.githubusercontent.com/Rikorose/DeepFilterNet/d375b2d8309e0935d165700c91da9de862a99c31/models/DeepFilterNet3_onnx.tar.gz).
- Archive SHA256: `c94d91f70911001c946e0fabb4aa9adc37045f45a03b56008cb0c8244cb63616`.
- `df3.f32`: 2,134,646 little-endian float32 values from the three official ONNX
  graphs, including their constant tensors and BatchNorm-folded convolutions.
- Tensor SHA256: `cdca621ee0a8986af2cc19e9e45cc45a093cccf57c4d3bf718cd8b910291a2e3`.
- Typed parameter constants and tensor offsets: `src/apo/dsp/deep_filter/weights.rs`.
- The original training/model configuration is retained in `config.ini`.

The model operates at 48 kHz with a 960-point Vorbis STFT, 480-sample hop,
32 ERB bands, 96 deep-filter bins, five complex taps and two frames of lookahead.
The convolution width is 64 and the recurrent hidden width is 256. Intrinsic
STFT/model delay is 1,440 samples (30 ms), including when enhancement is bypassed.
Adapter buffering, resampling, common-delay alignment and final limiting add delay.

The APO embeds the weights and uses native synchronous Rust inference. It does
not require Python, ONNX Runtime, Tract, model downloads or an inference worker.
All models and their buffers are prepared before processing starts. Switching
runs one model at a time: 5 ms fade out, reset, silence while buffers fill,
and 5 ms fade in. All paths retain the same device-frame delay within a stream.

Enhancement follows the official libDF default SNR thresholds and disables its
optional postfilter. Unlike upstream's sustained-silence early return, VoiceBox
continues advancing the STFT, spectrum history and feature normalization while
skipping neural inference and emitting silence. This prevents stale overlap
audio from returning after a pause.

The raw asset retains the official ONNX tensor ordering. Its little-endian
values are decoded with `f32::from_le_bytes` during compilation. Rust references it
through typed `ENCODER`, `ERB_DECODER` and `DF_DECODER` constants; no parameter
names or tensor lookup table are used at runtime. Slice bounds, convolution
shapes, grouped linear sizes and GRU parameter sizes are checked during
constant evaluation. Updating the model requires exporting the new tensors
and updating these typed offsets together.
