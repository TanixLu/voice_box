//! A frame-clocked linear ramp shared by every channel of a stream.

pub struct Ramp {
    current: f32,
    target: f32,
    step: f32,
    remaining: usize,
    frames: usize,
}

impl Ramp {
    pub fn new(value: f32, frames: usize) -> Self {
        Self {
            current: value,
            target: value,
            step: 0.0,
            remaining: 0,
            frames,
        }
    }

    pub fn reset(&mut self, value: f32) {
        self.current = value;
        self.target = value;
        self.step = 0.0;
        self.remaining = 0;
    }

    pub fn set_target(&mut self, target: f32) {
        if target != self.target {
            self.target = target;
            self.remaining = self.frames;
            self.step = (target - self.current) / self.frames as f32;
        }
    }

    pub fn next(&mut self) -> f32 {
        if self.remaining != 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            } else {
                self.current += self.step;
            }
        }
        self.current
    }
}
