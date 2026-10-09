Vendored crates.io `aec3` 0.4.0 (https://github.com/RubyBit/aec3-rs).

VoiceBox uses the low-level AEC3 modules, not the allocating graph/frame wrappers.
In `src/audio_processing/aec3/aec3_fft.rs`, replace
RustFFT `process` with `process_with_scratch` and fixed 128-element stack scratch.
Plan scratch bounds are validated during construction. This preserves FFT output
while removing per-transform heap allocations from the audiodg real-time path.

In `src/audio_processing/aec3/matched_filter.rs`, compile diagnostic name
formatting only with the `diagnostics` feature. VoiceBox leaves that feature off:
even disabled diagnostics must not allocate strings during delay estimation.

VoiceBox supplies block coordination and reuses existing allocations on reset.
Unused graph pipelines, frame wrappers, AGC/NS modules and development code have
been removed. Upstream licensing and patent notices are preserved.
