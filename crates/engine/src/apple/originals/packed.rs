//! Bounded lossless float fallback for legacy caches and resources without encoded bytes.
use rayon::prelude::*;
use std::io::{Read, Write};

pub(super) const PIXELS: usize = 65_536;
const GROUP: usize = 8;
pub(super) const OVERHEAD: usize = 4096;

fn pack(pixels: &[[f32; 3]]) -> Vec<u8> {
    let n = pixels.len();
    if n == 0 {
        return Vec::new();
    }
    let mut planes = vec![0; n * 12];
    for (channel, target) in planes.chunks_exact_mut(n * 4).enumerate() {
        let mut previous = 0;
        for (i, pixel) in pixels.iter().enumerate() {
            let bits = pixel.get(channel).copied().unwrap_or_default().to_bits();
            for (plane, byte) in target.chunks_exact_mut(n).zip((bits ^ previous).to_le_bytes()) {
                if let Some(slot) = plane.get_mut(i) {
                    *slot = byte;
                }
            }
            previous = bits;
        }
    }
    miniz_oxide::deflate::compress_to_vec_zlib(&planes, 3)
}

fn unpack(bytes: &[u8], count: usize) -> Result<Vec<[f32; 3]>, String> {
    if count == 0 || count > PIXELS {
        return Err("Invalid Photos block dimensions".into());
    }
    let planes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes, count * 12).map_err(|_| "Corrupt compressed Photos pixels")?;
    if planes.len() != count * 12 {
        return Err("Truncated compressed Photos pixels".into());
    }
    let mut pixels = vec![[0.0; 3]; count];
    for (channel, source) in planes.chunks_exact(count * 4).enumerate() {
        let mut previous = 0;
        for (i, pixel) in pixels.iter_mut().enumerate() {
            let mut delta = 0;
            for (shift, plane) in source.chunks_exact(count).enumerate() {
                delta |= u32::from(*plane.get(i).ok_or("Truncated Photos byte plane")?) << (shift * 8);
            }
            let bits = delta ^ previous;
            let value = f32::from_bits(bits);
            if !value.is_finite() {
                return Err("Invalid cached Photos pixel".into());
            }
            *pixel.get_mut(channel).ok_or("Invalid Photos channel")? = value;
            previous = bits;
        }
    }
    Ok(pixels)
}

pub(super) fn write(writer: &mut dyn Write, pixels: &[[f32; 3]]) -> std::io::Result<u64> {
    let mut total = 0;
    for group in pixels.chunks(PIXELS * GROUP) {
        let blocks: Vec<_> = group.par_chunks(PIXELS).map(pack).collect();
        for block in blocks {
            writer.write_all(&(block.len() as u32).to_le_bytes())?;
            writer.write_all(&block)?;
            total += 4 + block.len() as u64;
        }
    }
    Ok(total)
}

pub(super) fn read(reader: &mut dyn Read, count: usize) -> Result<Vec<[f32; 3]>, String> {
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(count).map_err(|_| "Not enough memory for cached Photos pixels")?;
    while pixels.len() < count {
        let mut blocks = Vec::new();
        let mut left = count - pixels.len();
        for _ in 0..GROUP {
            if left == 0 {
                break;
            }
            let n = left.min(PIXELS);
            let mut size = [0; 4];
            reader.read_exact(&mut size).map_err(|e| e.to_string())?;
            let size = u32::from_le_bytes(size) as usize;
            if size < 6 || size > n * 12 + OVERHEAD {
                return Err("Invalid compressed Photos block size".into());
            }
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            blocks.push((n, bytes));
            left -= n;
        }
        let decoded: Result<Vec<_>, _> = blocks.par_iter().map(|(n, bytes)| unpack(bytes, *n)).collect();
        for mut block in decoded? {
            pixels.append(&mut block);
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_bits_survive_multiple_blocks_including_negative_zero_and_hdr() {
        let pixels: Vec<_> = (0..PIXELS + 13).map(|i| [f32::from_bits(0x3e000000 + i as u32), -0.0, 2.125]).collect();
        let mut bytes = Vec::new();
        write(&mut bytes, &pixels).unwrap();
        let decoded = read(&mut bytes.as_slice(), pixels.len()).unwrap();
        assert!(pixels.iter().flatten().zip(decoded.iter().flatten()).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(bytes.len() < pixels.len() * 4);
        let mut bad = bytes.clone();
        bad[4] ^= 0x5a;
        assert!(read(&mut bad.as_slice(), pixels.len()).is_err());
        assert!(read(&mut &bytes[..bytes.len() - 1], pixels.len()).is_err());
        assert!(read(&mut [255u8; 4].as_slice(), pixels.len()).is_err());
    }
}
