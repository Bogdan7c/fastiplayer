/// Simple big-endian bit reader for VP9 uncompressed header.
pub struct BitReader<'a> {
    data: &'a [u8],
    byte_offset: usize,
    bit_offset: u8, // 0..7, current bit within byte (MSB first)
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_offset: 0,
            bit_offset: 0,
        }
    }

    pub fn read_bit(&mut self) -> Option<u8> {
        if self.byte_offset >= self.data.len() {
            return None;
        }
        let byte = self.data[self.byte_offset];
        let bit = (byte >> (7 - self.bit_offset)) & 1;
        self.bit_offset += 1;
        if self.bit_offset == 8 {
            self.bit_offset = 0;
            self.byte_offset += 1;
        }
        Some(bit)
    }

    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut val = 0u32;
        for _ in 0..n {
            val = (val << 1) | (self.read_bit()? as u32);
        }
        Some(val)
    }

    pub fn read_literal(&mut self, n: u8) -> Option<u32> {
        self.read_bits(n)
    }

    pub fn eof(&self) -> bool {
        self.byte_offset >= self.data.len()
    }
}
