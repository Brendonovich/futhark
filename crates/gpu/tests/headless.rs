use futhark_gpu::Gpu;

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
