# VoiceBox AEC3 DSP subset

Source: [`aec3` 0.4.0 / RubyBit/aec3-rs](https://github.com/RubyBit/aec3-rs).

Only the low-level AEC3 modules used by VoiceBox, their configuration, and their
DSP/diagnostic dependencies are retained. Graph pipelines, frame wrappers,
AGC, noise suppression, standalone resamplers, examples and development tools
have been removed. VoiceBox provides its own block coordination, resampling and
GTCRN speech enhancement.

Local real-time changes are described in [VOICEBOX-PATCH.md](VOICEBOX-PATCH.md).
Upstream licensing and patent notices are preserved in LICENSE and PATENT.
