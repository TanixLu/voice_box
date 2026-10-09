Vendored crates.io `aec3` 0.4.0 (https://github.com/RubyBit/aec3-rs).

VoiceBox uses the low-level AEC3 modules, not the allocating graph/frame wrappers.
In `src/audio_processing/aec3/aec3_fft.rs`, replace
RustFFT `process` with `process_with_scratch` and fixed 128-element stack scratch.
Plan scratch bounds are validated during construction. This preserves FFT output
while removing per-transform heap allocations from the audiodg real-time path.

Diagnostic dumping, logging and their dependencies have been removed, including
diagnostic name formatting during delay estimation.

Unused metrics reporting, ERL reporting-only estimation, activity/jitter
counters, external-delay APIs, linear-output export and frame-level configuration
have also been removed. ERLE and instantaneous filter-quality estimation used
by the residual echo suppressor are retained. The block DSP and CPU fallbacks
remain; only the modules needed by VoiceBox's adapter are public.

The adapter no longer calls the empty render-call logging method or passes
the ignored render-buffer delay argument to the delay controller.

VoiceBox supplies block coordination and reuses existing allocations on reset.
Unused graph pipelines, frame wrappers, AGC/NS modules and development code have
been removed. The remaining DSP dependencies are num-complex and rustfft.
Upstream licensing and patent notices are preserved in LICENSE and PATENT.
