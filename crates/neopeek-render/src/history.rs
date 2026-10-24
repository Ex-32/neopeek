//! The `MAX_BINS`×`depth` `R32Float` history texture ring (PLAN.md §4.1).
//!
//! One `write_texture` of the newest magnitude row (~4 KB) per slice; the
//! ring wrap is resolved at *sample* time from a uniform head index
//! (`grid::history_row`), so no CPU row shifting ever happens. The texture
//! is re-allocated when the depth setting changes.
//!
//! The FFT size is runtime-adjustable, so the texture is allocated once
//! at the *maximum* width (`neopeek_dsp::MAX_BINS` = 4096; 4096·48·4 ≈
//! 786 KB at the default depth — cheaper than a reallocation dance on
//! every `o`/`n` press) and only the first `frame.bin_count` columns are
//! written and read: the vertex grid spans exactly the valid bins, so
//! shader `textureLoad` coordinates never reach the unused tail. Columns
//! beyond `bin_count` may hold stale magnitudes from a previous, larger
//! geometry — they are never sampled while `bin_count` is the current
//! width. Resize-on-change was rejected: it re-allocates the texture and
//! rebuilds every bind group on each size key.

use neopeek_dsp::{Frame, MAX_BINS};

/// Byte stride of one history row for a valid bin count. wgpu requires a
/// 256-multiple for `COPY_DST` rows; every ladder bin count × 4 (512·4 =
/// 1024 … 8192·4 = 16384) qualifies.
pub fn row_bytes(bin_count: u32) -> u32 {
    debug_assert!(
        bin_count.is_multiple_of(256),
        "row stride must stay a 256-multiple for wgpu COPY_DST: {bin_count}"
    );
    bin_count * 4
}

pub struct History {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// History layer count (texture height); `grid::clamp_depth` bounds it.
    pub depth: usize,
    /// Index of the next row to be written (`grid::history_row` resolves
    /// slice→row from this).
    head: usize,
}

impl History {
    pub fn new(device: &wgpu::Device, depth: usize) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neopeek-history"),
            size: wgpu::Extent3d {
                width: MAX_BINS as u32,
                height: depth as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Non-filterable float is enough: samples use `textureLoad`.
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            depth,
            head: 0,
        }
    }

    /// Writes one Frame's magnitude spectrum as the newest row and
    /// advances the head. Zero CPU copy past the write (~4 KB); only the
    /// first `frame.bin_count` columns are written (see module docs).
    pub fn write_slice(&mut self, queue: &wgpu::Queue, frame: &Frame) {
        let bin_count = frame.bin_count;
        debug_assert!(bin_count as usize <= MAX_BINS, "bin_count overflows the texture");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: self.head as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&frame.bins),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes(bin_count)),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: bin_count,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.head = (self.head + 1) % self.depth;
    }

    /// Row index the shader will use to look up a slice `s` slices ago
    /// (0 = newest). Exposed for tests + diagnostics.
    pub fn row_of(&self, slice: usize) -> usize {
        crate::grid::history_row(self.head, slice, self.depth)
    }

    pub fn head(&self) -> usize {
        self.head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::history_row;

    /// Ring-wrap invariant over `3n` writes for a layer count `n` (head
    /// updates mirror `write_slice`: write at head, then advance).
    fn walk_and_wrap(n: usize) {
        let mut head = 0usize;
        for i in 0..3 * n {
            // Newest slice (s = 0) must land on the just-written row.
            let just_written = (head + n - 1) % n;
            assert_eq!(history_row(head, 0, n), just_written);
            assert_eq!(history_row(head, 0, n), (i + n - 1) % n);
            // One slice older than newest = the previous written row.
            if i > 0 {
                assert_eq!(history_row(head, 1, n), (i + n - 2) % n);
            }
            head = (head + 1) % n;
        }
        assert_eq!(head, 0, "full wrap restores the start head");
    }

    #[test]
    fn write_slice_row_walks_and_wraps() {
        walk_and_wrap(crate::grid::DEFAULT_DEPTH);
    }

    /// Non-default (non-pow-2, odd) layer count must wrap identically —
    /// the depth setting is runtime-adjustable.
    #[test]
    fn wrap_works_at_non_default_depth() {
        walk_and_wrap(13);
        walk_and_wrap(crate::grid::MIN_DEPTH);
        walk_and_wrap(crate::grid::MAX_DEPTH);
    }

    /// Every ladder size's row stride stays a 256-multiple (the wgpu
    /// `COPY_DST` `bytes_per_row` requirement) — so `write_slice` can
    /// write exactly `bin_count` columns at any FFT size without an
    /// aligned-stride dance.
    #[test]
    fn row_bytes_are_wgpu_aligned_for_every_ladder_size() {
        for size in neopeek_dsp::FFT_SIZES {
            let bin_count = (size / 2) as u32;
            assert!(row_bytes(bin_count).is_multiple_of(256), "fft {size}");
            assert_eq!(row_bytes(bin_count), bin_count * 4);
        }
        // The default and the texture bound.
        assert_eq!(row_bytes(neopeek_dsp::DEFAULT_BINS as u32), 4096);
        assert_eq!(row_bytes(neopeek_dsp::MAX_BINS as u32), 16_384);
    }
}
