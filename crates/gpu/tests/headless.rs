use futhark_gpu::{Fit, Gpu, TextureKey, TexturePool};

#[test]
fn clears_and_reads_back_a_texture() {
    let gpu = Gpu::headless().expect("a GPU adapter (on Linux CI, lavapipe)");
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: 70,
            height: 3,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 1.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    gpu.queue.submit([encoder.finish()]);

    // 70 pixels wide makes a row 280 bytes, which needs padding to 256-byte alignment on readback.
    let pixels = gpu.read_rgba8(&texture).unwrap();
    assert_eq!(pixels.len(), 70 * 3 * 4);
    assert!(pixels.chunks(4).all(|p| p == [255, 0, 255, 255]));
}

fn texture(gpu: &Gpu, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: TexturePool::USAGE,
        view_formats: &[],
    })
}

/// A 2×1 source, red then green, uploaded as `format`.
fn red_green(gpu: &Gpu, format: wgpu::TextureFormat, texel: &[[u8; 16]; 2]) -> wgpu::Texture {
    let source = texture(gpu, 2, 1, format);
    let size = format.block_copy_size(None).unwrap() as usize;
    let data: Vec<u8> = texel.iter().flat_map(|t| t[..size].to_vec()).collect();
    gpu.queue.write_texture(
        source.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(2 * size as u32),
            rows_per_image: None,
        },
        source.size(),
    );
    source
}

/// Draws a 2×1 source into an 8×8 `Bgra8Unorm` target and returns the target's BGRA pixels by row.
fn draw(gpu: &Gpu, source: &wgpu::Texture, fit: Fit) -> Vec<Vec<[u8; 4]>> {
    let target = texture(gpu, 8, 8, wgpu::TextureFormat::Bgra8Unorm);
    let scope = gpu.error_scope();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    gpu.draw_scaled(
        &mut encoder,
        &source.create_view(&Default::default()),
        &target.create_view(&Default::default()),
        fit,
    );
    gpu.queue.submit([encoder.finish()]);
    scope.finish().unwrap();
    gpu.read_texture(&target)
        .unwrap()
        .chunks(32)
        .map(|row| row.chunks(4).map(|p| p.try_into().unwrap()).collect())
        .collect()
}

#[test]
fn draws_scaled_into_a_bgra_target() {
    let gpu = Gpu::headless().unwrap();
    let mut red_green_8 = [[0; 16]; 2];
    red_green_8[0][..4].copy_from_slice(&[255, 0, 0, 255]);
    red_green_8[1][..4].copy_from_slice(&[0, 255, 0, 255]);
    let mut red_green_32 = [[0; 16]; 2];
    for (texel, rgba) in red_green_32
        .iter_mut()
        .zip([[1.0f32, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]])
    {
        texel.copy_from_slice(&rgba.map(f32::to_le_bytes).concat());
    }
    let sources = [
        red_green(&gpu, wgpu::TextureFormat::Rgba8Unorm, &red_green_8),
        // Not filterable, so it is drawn with a nearest sampler.
        red_green(&gpu, wgpu::TextureFormat::Rgba32Float, &red_green_32),
    ];
    const RED: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    for source in &sources {
        // 2:1 into 1:1 leaves two transparent rows above and below.
        let rows = draw(&gpu, source, Fit::Contain);
        for (y, row) in rows.iter().enumerate() {
            if !(2..6).contains(&y) {
                assert!(row.iter().all(|p| *p == [0; 4]), "row {y}: {row:?}");
            } else {
                assert_eq!((row[0], row[7]), (RED, GREEN), "row {y}");
                assert!(row.iter().all(|p| p[3] == 255), "row {y}: {row:?}");
            }
        }

        let rows = draw(&gpu, source, Fit::Stretch);
        assert!(rows.iter().all(|row| row[0] == RED && row[7] == GREEN));
        // Covering crops to the middle, where the two colors meet.
        let rows = draw(&gpu, source, Fit::Cover);
        assert!(rows.iter().all(|row| row.iter().all(|p| p[3] == 255)));
    }
}

#[test]
fn the_pool_reuses_dropped_textures() {
    let gpu = Gpu::headless().unwrap();
    let mut pool = TexturePool::default();
    let key = TextureKey {
        width: 4,
        height: 4,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: TexturePool::USAGE,
    };
    let a = pool.acquire(&gpu.device, key);
    let b = pool.acquire(&gpu.device, key);
    assert_ne!(a, b);
    assert_eq!(pool.len(), 2);

    drop(a);
    let c = pool.acquire(&gpu.device, key);
    assert_eq!(pool.len(), 2);
    let other = pool.acquire(&gpu.device, TextureKey { width: 8, ..key });
    assert_eq!(pool.len(), 3);

    // Free textures go after a few unused generations.
    drop((b, other));
    for _ in 0..3 {
        pool.trim();
    }
    assert_eq!(pool.len(), 1);
    drop(c);
}
