use egui::{UserData, ViewportId};
use epaint::ColorImage;
use std::sync::{mpsc, Arc, Mutex};
use wgpu::{BindGroupLayout, MultisampleState, StoreOp};

/// A texture and a buffer for reading the rendered frame back to the cpu.
/// The texture is required since [`wgpu::TextureUsages::COPY_SRC`] is not an allowed
/// flag for the surface texture on all platforms. This means that anytime we want to
/// capture the frame, we first render it to this texture, and then we can copy it to
/// both the surface texture (via a render pass) and the buffer (via a texture to buffer copy),
/// from where we can pull it back
/// to the cpu.
pub struct CaptureState {
    available_buffer: Arc<Mutex<Option<wgpu::Buffer>>>,
    padding: BufferPadding,
    pub texture: wgpu::Texture,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

pub type CaptureReceiver = mpsc::Receiver<(ViewportId, Vec<UserData>, ColorImage)>;
pub type CaptureSender = mpsc::Sender<(ViewportId, Vec<UserData>, ColorImage)>;
pub use mpsc::channel as capture_channel;

impl CaptureState {
    pub fn new(device: &wgpu::Device, surface_texture: &wgpu::Texture) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("texture_copy.wgsl"));

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("texture_copy"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(surface_texture.format().into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let bind_group_layout = pipeline.get_bind_group_layout(0);

        let (texture, padding, bind_group) =
            Self::create_texture(device, surface_texture, &bind_group_layout);

        Self {
            available_buffer: Default::default(),
            padding,
            texture,
            pipeline,
            bind_group,
        }
    }

    fn create_texture(
        device: &wgpu::Device,
        surface_texture: &wgpu::Texture,
        layout: &BindGroupLayout,
    ) -> (wgpu::Texture, BufferPadding, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui_screen_capture_texture"),
            size: surface_texture.size(),
            mip_level_count: surface_texture.mip_level_count(),
            sample_count: surface_texture.sample_count(),
            dimension: surface_texture.dimension(),
            format: surface_texture.format(),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let padding = BufferPadding::new(surface_texture.width());

        let view = texture.create_view(&Default::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            }],
            label: None,
        });

        (texture, padding, bind_group)
    }

    /// Updates the [`CaptureState`] if the size of the surface texture has changed
    pub fn update(&mut self, device: &wgpu::Device, texture: &wgpu::Texture) {
        if self.texture.size() != texture.size() {
            let (new_texture, padding, bind_group) =
                Self::create_texture(device, texture, &self.pipeline.get_bind_group_layout(0));
            self.texture = new_texture;
            self.padding = padding;
            self.bind_group = bind_group;
        }
    }

    /// Handles copying from the [`CaptureState`] texture to the surface texture and the buffer.
    /// Pass the returned buffer to [`CaptureState::read_screen_rgba`] to read the data back to the cpu.
    pub fn copy_textures(
        &mut self,
        device: &wgpu::Device,
        output_frame: &wgpu::SurfaceTexture,
        encoder: &mut wgpu::CommandEncoder,
    ) -> wgpu::Buffer {
        debug_assert_eq!(
            self.texture.size(),
            output_frame.texture.size(),
            "Texture sizes must match, `CaptureState::update` was probably not called"
        );

        let buffer = self.copy_to_buffer(device, encoder);

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("texture_copy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output_frame.texture.create_view(&Default::default()),
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);

        buffer
    }

    fn copy_to_buffer(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
    ) -> wgpu::Buffer {
        let size = u64::from(self.padding.padded_bytes_per_row) * u64::from(self.texture.height());
        // 只复用已解除映射且大小匹配的缓冲；上一帧仍在读回时照常提交新帧。
        let available = self.available_buffer.lock().unwrap().take();
        let buffer = available.filter(|b| b.size() == size).unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("egui_screen_capture_buffer"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padding.padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            self.texture.size(),
        );

        buffer
    }

    /// Handles copying from the [`CaptureState`] texture to the surface texture and the cpu
    /// This function is non-blocking and will send the data to the given sender when it's ready.
    /// Pass in the buffer returned from [`CaptureState::copy_textures`].
    /// Make sure to call this after the encoder has been submitted.
    pub fn read_screen_rgba(
        &self,
        ctx: egui::Context,
        buffer: wgpu::Buffer,
        data: Vec<UserData>,
        tx: CaptureSender,
        viewport_id: ViewportId,
    ) {
        let available = self.available_buffer.clone();
        let buffer_clone = buffer.clone();
        let buffer_slice = buffer_clone.slice(..);
        let format = self.texture.format();
        let tex_extent = self.texture.size();
        let padding = self.padding;
        let bgra = match format {
            wgpu::TextureFormat::Rgba8Unorm => false,
            wgpu::TextureFormat::Bgra8Unorm => true,
            _ => {
                log::error!("Screen can't be captured unless the surface format is Rgba8Unorm or Bgra8Unorm. Current surface format is {:?}", format);
                return;
            }
        };
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            if let Err(err) = result {
                log::error!("Failed to map buffer for reading: {:?}", err);
                return;
            }
            let buffer_slice = buffer.slice(..);

            let pixels = capture_pixels(&buffer_slice.get_mapped_range(), padding, bgra);
            buffer.unmap();
            *available.lock().unwrap() = Some(buffer);

            tx.send((
                viewport_id,
                data,
                ColorImage {
                    size: [tex_extent.width as usize, tex_extent.height as usize],
                    pixels,
                },
            ))
            .ok();
            // 岛仔：交付截图只需要一帧。request_repaint() 会连画两帧，兼容渲染每次读回都要多画一帧；
            // 非零延迟只触发一次（egui 会再扣掉一帧的预测时长，实际仍是下一帧就画）。
            ctx.request_repaint_after(std::time::Duration::from_millis(1));
        });
    }
}

#[derive(Copy, Clone)]
struct BufferPadding {
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
}

fn capture_pixels(mapped: &[u8], padding: BufferPadding, bgra: bool) -> Vec<epaint::Color32> {
    let rows = mapped.chunks_exact(padding.padded_bytes_per_row as usize);
    let width_bytes = padding.unpadded_bytes_per_row as usize;
    let mut pixels = Vec::with_capacity(rows.len() * (width_bytes / 4));
    for row in rows {
        let row = &row[..width_bytes];
        if bgra {
            let colors: &[[u8; 4]] = bytemuck::cast_slice(row);
            pixels.extend(colors.iter().map(|&[b, g, r, a]| epaint::Color32::from_rgba_premultiplied(r, g, b, a)));
        } else {
            pixels.extend_from_slice(bytemuck::cast_slice(row));
        }
    }
    pixels
}

#[cfg(test)]
#[test]
fn captures_both_channel_orders_without_row_padding() {
    let padding = BufferPadding { unpadded_bytes_per_row: 4, padded_bytes_per_row: 8 };
    let rows = [8, 4, 2, 16, 99, 99, 99, 99, 3, 6, 9, 12, 77, 77, 77, 77];
    for (bgra, expected) in [
        (false, [[8, 4, 2, 16], [3, 6, 9, 12]]),
        (true, [[2, 4, 8, 16], [9, 6, 3, 12]]),
    ] {
        let pixels = capture_pixels(&rows, padding, bgra);
        assert_eq!(bytemuck::cast_slice::<_, [u8; 4]>(&pixels), &expected);
    }
}

impl BufferPadding {
    fn new(width: u32) -> Self {
        let bytes_per_pixel = std::mem::size_of::<u32>() as u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let padded_bytes_per_row =
            wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        Self {
            unpadded_bytes_per_row,
            padded_bytes_per_row,
        }
    }
}
