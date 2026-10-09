//! Allocation-free 16 kHz AEC3 block processing before the existing GTCRN.
//! The graph/frame wrappers allocate during processing, so use the DSP modules directly.
use crate::apo::reference_buffer::{BLOCK, CHANNELS, RATE, Reference};
use aec3::{
    api::config::EchoCanceller3Config,
    audio_processing::aec3::{
        block::Block,
        delay_estimate::DelayEstimate,
        echo_path_variability::{DelayAdjustment, EchoPathVariability},
        echo_remover::EchoRemover,
        render_delay_buffer::{BufferingEvent, RenderDelayBuffer},
        render_delay_controller::RenderDelayController,
    },
};

// The AEC3 suppression filter has one 64-sample overlap block of delay.
pub const LATENCY_SAMPLES: usize = BLOCK;
const PCM_SCALE: f32 = 32768.0;
const WARMUP_BLOCKS: usize = 125;
const MAX_RENDER_BURST: u64 = 32;

/// Same block coordination as upstream BlockProcessor, with an explicit reset
/// that reuses all allocations when devices, settings, or capture streams change.
struct Core {
    render: RenderDelayBuffer,
    delay: RenderDelayController,
    remover: EchoRemover,
    event: BufferingEvent,
    changed: bool,
}

impl Core {
    fn new() -> Self {
        let config = EchoCanceller3Config::default();
        Self {
            render: RenderDelayBuffer::new(config.clone(), RATE as i32, CHANNELS),
            delay: RenderDelayController::new(&config, RATE as i32, 1),
            remover: EchoRemover::new(config, RATE as i32, CHANNELS, 1),
            event: BufferingEvent::None,
            changed: true,
        }
    }
    fn reset(&mut self) {
        self.render.reset();
        self.delay.reset(true);
        self.event = BufferingEvent::None;
        self.changed = true;
    }
    fn render(&mut self, block: &Block) {
        self.event = self.render.insert(block);
    }
    fn capture(&mut self, saturated: bool, block: &mut Block) {
        let mut variation = EchoPathVariability::new(self.changed, DelayAdjustment::None, false);
        if self.changed || matches!(self.event, BufferingEvent::RenderOverrun) {
            variation.delay_change = DelayAdjustment::BufferFlush;
            self.delay.reset(true);
        }
        self.changed = false;
        self.event = BufferingEvent::None;
        if matches!(
            self.render.prepare_capture_processing(),
            BufferingEvent::RenderUnderrun
        ) {
            self.delay.reset(false);
        }
        let estimate: Option<DelayEstimate> = self
            .delay
            .get_delay(self.render.downsampled_render_buffer(), block);
        if let Some(estimate) = estimate
            && self.render.align_from_delay(estimate.delay)
        {
            variation.delay_change = DelayAdjustment::NewDetectedDelay;
        }
        variation.clock_drift = self.delay.has_clockdrift();
        self.remover.process_capture(
            variation,
            saturated,
            estimate,
            &self.render.render_buffer(),
            block,
        );
    }
}

pub(super) struct AecChannel {
    core: Core,
    render: Block,
    capture: Block,
    reference: [[f32; BLOCK]; CHANNELS],
    dry_delay: [f32; BLOCK],
    dry_position: usize,
    epoch: u64,
    next: u64,
    active: bool,
    warmup: usize,
}
impl AecChannel {
    pub fn new() -> Self {
        Self {
            core: Core::new(),
            render: Block::new(1, CHANNELS),
            capture: Block::new(1, 1),
            reference: [[0.0; BLOCK]; CHANNELS],
            dry_delay: [0.0; BLOCK],
            dry_position: 0,
            epoch: 0,
            next: 0,
            active: false,
            warmup: WARMUP_BLOCKS,
        }
    }
    pub fn reset(&mut self) {
        self.core.reset();
        self.dry_delay.fill(0.0);
        self.dry_position = 0;
        self.active = false;
        self.next = 0;
        self.warmup = WARMUP_BLOCKS;
    }
    pub fn process(&mut self, samples: &mut [f32], enabled: bool, reference: Option<&Reference>) {
        let snapshot = enabled
            .then(|| reference.and_then(Reference::latest))
            .flatten();
        if let Some((epoch, latest)) = snapshot {
            if !self.active
                || epoch != self.epoch
                || latest.saturating_sub(self.next) > MAX_RENDER_BURST
            {
                self.core.reset();
                self.epoch = epoch;
                self.next = latest
                    .saturating_sub((samples.len() / BLOCK) as u64 - 1)
                    .max(1);
                self.warmup = WARMUP_BLOCKS;
            }
            self.active = true;
            // Drain the asynchronous render stream, bounded even after a stalled callback.
            for _ in 0..=MAX_RENDER_BURST {
                if self.next > latest {
                    break;
                }
                if reference
                    .unwrap()
                    .read(epoch, self.next, &mut self.reference)
                    .is_none()
                {
                    self.active = false;
                    break;
                }
                for channel in 0..CHANNELS {
                    for (dest, &sample) in self
                        .render
                        .view_mut(0, channel)
                        .iter_mut()
                        .zip(&self.reference[channel])
                    {
                        *dest = sample * PCM_SCALE;
                    }
                }
                self.core.render(&self.render);
                let Some(next) = self.next.checked_add(1) else {
                    self.active = false;
                    break;
                };
                self.next = next;
            }
        } else {
            self.active = false;
        }
        for block in samples.as_chunks_mut::<BLOCK>().0 {
            let mut dry = [0.0; BLOCK];
            for (index, &sample) in block.iter().enumerate() {
                dry[index] = self.dry_delay[self.dry_position];
                self.dry_delay[self.dry_position] = sample;
                self.dry_position = (self.dry_position + 1) % BLOCK;
                self.capture.view_mut(0, 0)[index] = sample * PCM_SCALE;
            }
            if self.active {
                let saturated = block.iter().any(|sample| sample.abs() >= 0.998);
                self.core.capture(saturated, &mut self.capture);
                if self.warmup == 0 {
                    for (dest, &sample) in block.iter_mut().zip(self.capture.view(0, 0)) {
                        *dest = if sample.is_finite() {
                            sample / PCM_SCALE
                        } else {
                            0.0
                        };
                    }
                    continue;
                }
                self.warmup -= 1;
            }
            block.copy_from_slice(&dry);
        }
    }
}
