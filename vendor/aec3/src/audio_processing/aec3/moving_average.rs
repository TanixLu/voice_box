/// Simple moving average over vectors with fixed length.
pub struct MovingAverage {
    num_elements: usize,
    memory: Vec<f32>,
    history_len: usize,
    scaling: f32,
    mem_index: usize,
}

impl MovingAverage {
    pub fn new(num_elements: usize, window_length: usize) -> Self {
        assert!(
            num_elements > 0,
            "moving average requires at least one element"
        );
        assert!(
            window_length > 0,
            "moving average window length must be positive"
        );
        let history_len = window_length.saturating_sub(1);
        let memory = vec![0.0; num_elements * history_len];
        let scaling = 1.0 / window_length as f32;
        Self {
            num_elements,
            memory,
            history_len,
            scaling,
            mem_index: 0,
        }
    }

    pub fn average(&mut self, input: &[f32], output: &mut [f32]) {
        assert_eq!(input.len(), self.num_elements);
        assert_eq!(output.len(), self.num_elements);

        output.copy_from_slice(input);
        if self.history_len > 0 {
            for chunk in self.memory.chunks_exact(self.num_elements) {
                for (dst, &value) in output.iter_mut().zip(chunk.iter()) {
                    *dst += value;
                }
            }
        }

        for value in output.iter_mut() {
            *value *= self.scaling;
        }

        if self.history_len == 0 {
            return;
        }

        let start = self.mem_index * self.num_elements;
        let end = start + self.num_elements;
        self.memory[start..end].copy_from_slice(input);
        self.mem_index = (self.mem_index + 1) % self.history_len;
    }
}
