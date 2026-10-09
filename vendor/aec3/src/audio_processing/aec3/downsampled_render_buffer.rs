pub struct DownsampledRenderBuffer {
    size: usize,
    pub buffer: Vec<f32>,
    pub write: usize,
    pub read: usize,
}

impl DownsampledRenderBuffer {
    pub fn new(size: usize) -> Self {
        assert!(size > 0);
        Self {
            size,
            buffer: vec![0.0; size],
            write: 0,
            read: 0,
        }
    }

    pub fn offset_index(&self, index: usize, offset: isize) -> usize {
        assert!(self.size > 0);
        let offset_abs = offset.abs() as usize;
        assert!(offset_abs <= self.size);
        let size = self.size as isize;
        let mut value = index as isize + offset;
        value %= size;
        if value < 0 {
            value += size;
        }
        value as usize
    }

    pub fn update_write_index(&mut self, offset: isize) {
        self.write = self.offset_index(self.write, offset);
    }

    pub fn update_read_index(&mut self, offset: isize) {
        self.read = self.offset_index(self.read, offset);
    }
}
