// TODO(ai-review): generated, not yet audited
//! The Vulkan side: a pipeline per device and swapchain format, and per swapchain image a
//! framebuffer, buffers and a submit that draws between the game's rendering and its present.
//! Everything runs on the game's own device and queue through the loader's real entry points.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::AtomicPtr;

use ash::vk;
use epaint::{Primitive, Vertex};

use super::paint::{Atlas, Paint};
use crate::{error, hooks, info, warn};

const LIBVULKAN: *const c_char = c"libvulkan.so.1".as_ptr();
const VERT_SPV: &[u8] = include_bytes!("shaders/overlay.vert.spv");
const FRAG_SPV: &[u8] = include_bytes!("shaders/overlay.frag.spv");
const MIN_BUFFER_SIZE: u64 = 64 * 1024;

static REAL_GET_DEVICE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GET_MEMORY_PROPERTIES: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REAL_GET_SURFACE_CAPABILITIES: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

static STATE: Mutex<Option<State>> = Mutex::new(None);

#[derive(Default)]
struct State {
    devices: HashMap<vk::Device, Device>,
    /// The device and queue family each queue was retrieved from.
    queues: HashMap<vk::Queue, (vk::Device, u32)>,
    swapchains: HashMap<vk::SwapchainKHR, Swapchain>,
}

struct Device {
    raw: ash::Device,
    physical: vk::PhysicalDevice,
    memory: vk::PhysicalDeviceMemoryProperties,
    get_swapchain_images: vk::PFN_vkGetSwapchainImagesKHR,
    pipelines: HashMap<vk::Format, Pipeline>,
    pools: HashMap<u32, vk::CommandPool>,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
}

struct Pipeline {
    render_pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    raw: vk::Pipeline,
}

struct Swapchain {
    device: vk::Device,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    targets: Vec<Option<Target>>,
}

/// What drawing into one swapchain image needs; reused once the image comes around again.
struct Target {
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    commands: vk::CommandBuffer,
    pool: vk::CommandPool,
    /// Signaled when the previous draw into this image finished.
    fence: vk::Fence,
    /// Signaled by the draw, waited on by the present.
    drawn: vk::Semaphore,
    vertices: Buffer,
    indices: Buffer,
    descriptors: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    atlas: Option<AtlasTexture>,
    staging: Buffer,
}

struct AtlasTexture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    size: [usize; 2],
    /// The atlas version the texture holds once every submitted draw ran.
    version: Option<u64>,
}

struct Buffer {
    raw: vk::Buffer,
    memory: vk::DeviceMemory,
    /// Persistently mapped host-coherent memory.
    mapped: usize,
    capacity: u64,
}

#[repr(C)]
struct Push {
    screen_size: [f32; 2],
    srgb_target: u32,
}

fn real<F>(cache: &AtomicPtr<c_void>, name: &CStr) -> F {
    let ptr = hooks::link_function(cache, name.as_ptr(), LIBVULKAN)
        .unwrap_or_else(|| panic!("{name:?} not found in libvulkan"));
    // SAFETY: the callers name loader exports whose signature is `F`.
    unsafe { std::mem::transmute_copy::<*mut c_void, F>(&ptr) }
}

/// The driver's function for `name` on `device`, past every hook.
fn device_proc_addr(device: vk::Device, name: &CStr) -> *const c_void {
    let get: vk::PFN_vkGetDeviceProcAddr = real(&REAL_GET_DEVICE_PROC_ADDR, c"vkGetDeviceProcAddr");
    // SAFETY: `device` is a live device created through the loader, `name` is NUL-terminated.
    match unsafe { get(device, name.as_ptr()) } {
        Some(f) => f as *const c_void,
        None => std::ptr::null(),
    }
}

pub(crate) fn on_create_device(physical: vk::PhysicalDevice, device: vk::Device) {
    let get_memory: vk::PFN_vkGetPhysicalDeviceMemoryProperties = real(
        &REAL_GET_MEMORY_PROPERTIES,
        c"vkGetPhysicalDeviceMemoryProperties",
    );
    let mut memory = vk::PhysicalDeviceMemoryProperties::default();
    // SAFETY: `physical` is the physical device `device` was just created from.
    unsafe { get_memory(physical, &mut memory) };
    // SAFETY: `device` is live; the loader hands out functions of the named signatures.
    let raw = unsafe { ash::Device::load_with(|name| device_proc_addr(device, name), device) };
    let get_swapchain_images = device_proc_addr(device, c"vkGetSwapchainImagesKHR");
    if get_swapchain_images.is_null() {
        info!(
            Overlay,
            "device {device:?} has no swapchain support, no overlay on it"
        );
        return;
    }
    // SAFETY: plain object creation on the live `device`.
    let (sampler, set_layout) = match unsafe { create_sampler_and_set_layout(&raw) } {
        Ok(created) => created,
        Err(e) => {
            error!(
                Overlay,
                "creating the atlas sampler on device {device:?} failed ({e:?}), no overlay on it"
            );
            return;
        }
    };
    let device_state = Device {
        raw,
        physical,
        memory,
        sampler,
        set_layout,
        // SAFETY: the loader answered for exactly this name.
        get_swapchain_images: unsafe {
            std::mem::transmute::<*const c_void, vk::PFN_vkGetSwapchainImagesKHR>(
                get_swapchain_images,
            )
        },
        pipelines: HashMap::new(),
        pools: HashMap::new(),
    };
    STATE
        .lock()
        .unwrap()
        .get_or_insert_default()
        .devices
        .insert(device, device_state);
}

pub(crate) fn on_get_device_queue(device: vk::Device, family: u32, queue: vk::Queue) {
    STATE
        .lock()
        .unwrap()
        .get_or_insert_default()
        .queues
        .insert(queue, (device, family));
}

/// `usage` plus color-attachment use, which drawing into the images needs, where the surface
/// allows it.
pub(crate) fn swapchain_usage(
    device: vk::Device,
    surface: vk::SurfaceKHR,
    usage: vk::ImageUsageFlags,
) -> vk::ImageUsageFlags {
    let state = STATE.lock().unwrap();
    let Some(physical) = state
        .as_ref()
        .and_then(|s| s.devices.get(&device))
        .map(|d| d.physical)
    else {
        return usage;
    };
    let get_capabilities: vk::PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR = real(
        &REAL_GET_SURFACE_CAPABILITIES,
        c"vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
    );
    let mut capabilities = vk::SurfaceCapabilitiesKHR::default();
    // SAFETY: `physical` and `surface` are the ones the swapchain is being created for.
    let result = unsafe { get_capabilities(physical, surface, &mut capabilities) };
    if result != vk::Result::SUCCESS {
        warn!(
            Overlay,
            "surface capabilities query failed ({result:?}), leaving swapchain usage as is"
        );
        return usage;
    }
    if capabilities
        .supported_usage_flags
        .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
    {
        usage | vk::ImageUsageFlags::COLOR_ATTACHMENT
    } else {
        usage
    }
}

pub(crate) fn on_create_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    info: &vk::SwapchainCreateInfoKHR,
) {
    let mut guard = STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else { return };
    let Some(device_state) = state.devices.get(&device) else {
        return;
    };
    if !info
        .image_usage
        .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
    {
        warn!(
            Overlay,
            "swapchain {swapchain:?} images are no color attachments, no overlay on it"
        );
        return;
    }
    let mut count = 0;
    let get = device_state.get_swapchain_images;
    // SAFETY: `swapchain` was just created on `device`.
    let images = unsafe {
        let result = get(device, swapchain, &mut count, std::ptr::null_mut());
        assert_eq!(result, vk::Result::SUCCESS, "count swapchain images");
        let mut images = vec![vk::Image::null(); count as usize];
        let result = get(device, swapchain, &mut count, images.as_mut_ptr());
        assert_eq!(result, vk::Result::SUCCESS, "get swapchain images");
        images
    };
    state.swapchains.insert(
        swapchain,
        Swapchain {
            device,
            format: info.image_format,
            extent: info.image_extent,
            targets: images.iter().map(|_| None).collect(),
            images,
        },
    );
}

pub(crate) fn on_destroy_swapchain(swapchain: vk::SwapchainKHR) {
    let mut guard = STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else { return };
    let Some(removed) = state.swapchains.remove(&swapchain) else {
        return;
    };
    let Some(device) = state.devices.get(&removed.device) else {
        return;
    };
    for target in removed.targets.into_iter().flatten() {
        // SAFETY: the target's objects belong to `device`; its last draw is waited for first.
        unsafe { destroy_target(&device.raw, target) };
    }
}

pub(crate) fn on_destroy_device(device: vk::Device) {
    let mut guard = STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else { return };
    let Some(removed) = state.devices.remove(&device) else {
        return;
    };
    state.queues.retain(|_, (d, _)| *d != device);
    let swapchains: Vec<_> = state
        .swapchains
        .iter()
        .filter(|(_, s)| s.device == device)
        .map(|(&h, _)| h)
        .collect();
    let raw = &removed.raw;
    // SAFETY: everything below was created on `device`, which the game is about to destroy.
    unsafe {
        for handle in swapchains {
            let swapchain = state.swapchains.remove(&handle).unwrap();
            for target in swapchain.targets.into_iter().flatten() {
                destroy_target(raw, target);
            }
        }
        for (_, pipeline) in removed.pipelines {
            raw.destroy_pipeline(pipeline.raw, None);
            raw.destroy_pipeline_layout(pipeline.layout, None);
            raw.destroy_render_pass(pipeline.render_pass, None);
        }
        for (_, pool) in removed.pools {
            raw.destroy_command_pool(pool, None);
        }
        raw.destroy_descriptor_set_layout(removed.set_layout, None);
        raw.destroy_sampler(removed.sampler, None);
    }
}

/// The size of `swapchain`'s images, when the overlay can draw into them through `queue`.
pub(super) fn extent(queue: vk::Queue, swapchain: vk::SwapchainKHR) -> Option<vk::Extent2D> {
    let guard = STATE.lock().unwrap();
    let state = guard.as_ref()?;
    let &(device, _) = state.queues.get(&queue)?;
    let swapchain = state.swapchains.get(&swapchain)?;
    state
        .devices
        .contains_key(&device)
        .then_some(swapchain.extent)
}

/// Draws `paint` into image `image_index` of `swapchain` after `wait`. Returns the semaphore the
/// present has to wait on instead, or None when the draw failed.
pub(super) fn draw(
    queue: vk::Queue,
    swapchain: vk::SwapchainKHR,
    image_index: u32,
    wait: &[vk::Semaphore],
    paint: &Paint,
) -> Option<vk::Semaphore> {
    let mut guard = STATE.lock().unwrap();
    let state = guard.as_mut()?;
    let &(device_handle, family) = state.queues.get(&queue)?;
    let swapchain_state = state.swapchains.get_mut(&swapchain)?;
    let device = state.devices.get_mut(&device_handle)?;

    // SAFETY: every handle below was created on `device`, which is live while it presents.
    match unsafe {
        record_and_submit(
            device,
            swapchain_state,
            family,
            queue,
            image_index,
            wait,
            paint,
        )
    } {
        Ok(drawn) => Some(drawn),
        Err(e) => {
            error!(
                Overlay,
                "drawing into swapchain {swapchain:?} image {image_index} failed: {e:?}"
            );
            None
        }
    }
}

unsafe fn record_and_submit(
    device: &mut Device,
    swapchain: &mut Swapchain,
    family: u32,
    queue: vk::Queue,
    image_index: u32,
    wait: &[vk::Semaphore],
    paint: &Paint,
) -> Result<vk::Semaphore, vk::Result> {
    let primitives = &paint.primitives;
    let format = swapchain.format;
    let extent = swapchain.extent;
    let image = swapchain.images[image_index as usize];
    // SAFETY: the caller's contract: all handles belong to `device`, `image_index` was acquired.
    unsafe {
        if !device.pipelines.contains_key(&format) {
            let pipeline = create_pipeline(&device.raw, format, device.set_layout)?;
            device.pipelines.insert(format, pipeline);
        }
        if !device.pools.contains_key(&family) {
            let pool = device.raw.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            device.pools.insert(family, pool);
        }
        let pipeline = &device.pipelines[&format];
        let slot = &mut swapchain.targets[image_index as usize];
        if slot.is_none() {
            *slot = Some(create_target(
                &device.raw,
                device.pools[&family],
                pipeline.render_pass,
                device.set_layout,
                image,
                format,
                extent,
            )?);
        }
        let target = slot.as_mut().unwrap();
        let raw = &device.raw;
        raw.wait_for_fences(&[target.fence], true, u64::MAX)?;
        raw.reset_fences(&[target.fence])?;

        let (vertex_count, index_count) =
            primitives
                .iter()
                .fold((0, 0), |(v, i), p| match &p.primitive {
                    Primitive::Mesh(mesh) => (v + mesh.vertices.len(), i + mesh.indices.len()),
                    Primitive::Callback(_) => (v, i),
                });
        let vertex_bytes = (vertex_count * size_of::<Vertex>()) as u64;
        let index_bytes = (index_count * size_of::<u32>()) as u64;
        ensure_capacity(
            raw,
            &device.memory,
            &mut target.vertices,
            vertex_bytes,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        )?;
        ensure_capacity(
            raw,
            &device.memory,
            &mut target.indices,
            index_bytes,
            vk::BufferUsageFlags::INDEX_BUFFER,
        )?;

        raw.reset_command_buffer(target.commands, vk::CommandBufferResetFlags::empty())?;
        raw.begin_command_buffer(
            target.commands,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        let uploaded = upload_atlas(raw, &device.memory, device.sampler, target, &paint.atlas)?;
        raw.cmd_begin_render_pass(
            target.commands,
            &vk::RenderPassBeginInfo::default()
                .render_pass(pipeline.render_pass)
                .framebuffer(target.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent,
                }),
            vk::SubpassContents::INLINE,
        );
        raw.cmd_bind_pipeline(
            target.commands,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.raw,
        );
        raw.cmd_bind_descriptor_sets(
            target.commands,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline.layout,
            0,
            &[target.descriptor_set],
            &[],
        );
        raw.cmd_set_viewport(
            target.commands,
            0,
            &[vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        let push = Push {
            screen_size: [extent.width as f32, extent.height as f32],
            srgb_target: is_srgb(format) as u32,
        };
        raw.cmd_push_constants(
            target.commands,
            pipeline.layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            std::slice::from_raw_parts((&raw const push).cast::<u8>(), size_of::<Push>()),
        );
        raw.cmd_bind_vertex_buffers(target.commands, 0, &[target.vertices.raw], &[0]);
        raw.cmd_bind_index_buffer(
            target.commands,
            target.indices.raw,
            0,
            vk::IndexType::UINT32,
        );

        let (mut first_vertex, mut first_index) = (0usize, 0usize);
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            std::ptr::copy_nonoverlapping(
                mesh.vertices.as_ptr(),
                (target.vertices.mapped as *mut Vertex).add(first_vertex),
                mesh.vertices.len(),
            );
            std::ptr::copy_nonoverlapping(
                mesh.indices.as_ptr(),
                (target.indices.mapped as *mut u32).add(first_index),
                mesh.indices.len(),
            );
            if let Some(scissor) = scissor(primitive.clip_rect, extent) {
                raw.cmd_set_scissor(target.commands, 0, &[scissor]);
                raw.cmd_draw_indexed(
                    target.commands,
                    mesh.indices.len() as u32,
                    1,
                    first_index as u32,
                    first_vertex as i32,
                    0,
                );
            }
            first_vertex += mesh.vertices.len();
            first_index += mesh.indices.len();
        }
        raw.cmd_end_render_pass(target.commands);
        raw.end_command_buffer(target.commands)?;

        let stages = vec![vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT; wait.len()];
        raw.queue_submit(
            queue,
            &[vk::SubmitInfo::default()
                .wait_semaphores(wait)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&[target.commands])
                .signal_semaphores(&[target.drawn])],
            target.fence,
        )?;
        if let Some(version) = uploaded {
            target.atlas.as_mut().unwrap().version = Some(version);
        }
        Ok(target.drawn)
    }
}

fn scissor(clip: epaint::Rect, extent: vk::Extent2D) -> Option<vk::Rect2D> {
    let min_x = clip.min.x.clamp(0.0, extent.width as f32).floor() as u32;
    let min_y = clip.min.y.clamp(0.0, extent.height as f32).floor() as u32;
    let max_x = clip.max.x.clamp(0.0, extent.width as f32).ceil() as u32;
    let max_y = clip.max.y.clamp(0.0, extent.height as f32).ceil() as u32;
    (max_x > min_x && max_y > min_y).then(|| vk::Rect2D {
        offset: vk::Offset2D {
            x: min_x as i32,
            y: min_y as i32,
        },
        extent: vk::Extent2D {
            width: max_x - min_x,
            height: max_y - min_y,
        },
    })
}

fn is_srgb(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::B8G8R8A8_SRGB
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::A8B8G8R8_SRGB_PACK32
            | vk::Format::R8G8B8_SRGB
            | vk::Format::B8G8R8_SRGB
    )
}

unsafe fn create_sampler_and_set_layout(
    raw: &ash::Device,
) -> Result<(vk::Sampler, vk::DescriptorSetLayout), vk::Result> {
    // SAFETY: plain object creation on a live device; every create info outlives its call.
    unsafe {
        let sampler = raw.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::LINEAR)
                .min_filter(vk::Filter::LINEAR)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
            None,
        )?;
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let set_layout = match raw.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding)),
            None,
        ) {
            Ok(layout) => layout,
            Err(e) => {
                raw.destroy_sampler(sampler, None);
                return Err(e);
            }
        };
        Ok((sampler, set_layout))
    }
}

unsafe fn create_pipeline(
    raw: &ash::Device,
    format: vk::Format,
    set_layout: vk::DescriptorSetLayout,
) -> Result<Pipeline, vk::Result> {
    // SAFETY: plain object creation on a live device; every create info outlives its call.
    unsafe {
        // The image arrives holding the game's frame for presentation and leaves the same way.
        let attachment = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let color = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color);
        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
        let render_pass = raw.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(std::slice::from_ref(&attachment))
                .subpasses(std::slice::from_ref(&subpass))
                .dependencies(std::slice::from_ref(&dependency)),
            None,
        )?;

        let push_range = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .size(size_of::<Push>() as u32);
        let layout = raw.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(std::slice::from_ref(&set_layout))
                .push_constant_ranges(std::slice::from_ref(&push_range)),
            None,
        )?;

        let vert = shader_module(raw, VERT_SPV)?;
        let frag = shader_module(raw, FRAG_SPV)?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(c"main"),
        ];
        let binding = vk::VertexInputBindingDescription::default()
            .stride(size_of::<Vertex>() as u32)
            .input_rate(vk::VertexInputRate::VERTEX);
        let attributes = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(std::mem::offset_of!(Vertex, pos) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(std::mem::offset_of!(Vertex, uv) as u32),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .format(vk::Format::R8G8B8A8_UNORM)
                .offset(std::mem::offset_of!(Vertex, color) as u32),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(std::slice::from_ref(&binding))
            .vertex_attribute_descriptions(&attributes);
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        // premultiplied color over the frame; the frame's alpha stays as the game wrote it
        let blend = vk::PipelineColorBlendAttachmentState::default()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(std::slice::from_ref(&blend));
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0);
        let created = raw.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
        raw.destroy_shader_module(vert, None);
        raw.destroy_shader_module(frag, None);
        let pipeline = created.map_err(|(_, e)| e)?[0];
        Ok(Pipeline {
            render_pass,
            layout,
            raw: pipeline,
        })
    }
}

unsafe fn shader_module(raw: &ash::Device, spv: &[u8]) -> Result<vk::ShaderModule, vk::Result> {
    let words: Vec<u32> = spv
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&w| u32::from_le_bytes(w))
        .collect();
    // SAFETY: `words` is a SPIR-V module compiled by glslc.
    unsafe { raw.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None) }
}

unsafe fn create_target(
    raw: &ash::Device,
    pool: vk::CommandPool,
    render_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    image: vk::Image,
    format: vk::Format,
    extent: vk::Extent2D,
) -> Result<Target, vk::Result> {
    // SAFETY: `image` is a swapchain image of `raw`, `pool` and `render_pass` were created on it.
    unsafe {
        let view = raw.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                }),
            None,
        )?;
        let framebuffer = raw.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(&[view])
                .width(extent.width)
                .height(extent.height)
                .layers(1),
            None,
        )?;
        let commands = raw.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        let fence = raw.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )?;
        let drawn = raw.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
        let pool_size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1);
        let descriptors = raw.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .max_sets(1)
                .pool_sizes(std::slice::from_ref(&pool_size)),
            None,
        )?;
        let descriptor_set = raw.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(descriptors)
                .set_layouts(std::slice::from_ref(&set_layout)),
        )?[0];
        Ok(Target {
            view,
            framebuffer,
            commands,
            pool,
            fence,
            drawn,
            vertices: Buffer::empty(),
            indices: Buffer::empty(),
            descriptors,
            descriptor_set,
            atlas: None,
            staging: Buffer::empty(),
        })
    }
}

unsafe fn destroy_target(raw: &ash::Device, target: Target) {
    // SAFETY: the target's objects belong to `raw` and are unused once its fence signaled.
    unsafe {
        if let Err(e) = raw.wait_for_fences(&[target.fence], true, u64::MAX) {
            error!(Overlay, "waiting for the last overlay draw failed: {e:?}");
        }
        target.vertices.destroy(raw);
        target.indices.destroy(raw);
        target.staging.destroy(raw);
        if let Some(atlas) = target.atlas {
            atlas.destroy(raw);
        }
        raw.destroy_descriptor_pool(target.descriptors, None);
        raw.destroy_semaphore(target.drawn, None);
        raw.destroy_fence(target.fence, None);
        raw.free_command_buffers(target.pool, &[target.commands]);
        raw.destroy_framebuffer(target.framebuffer, None);
        raw.destroy_image_view(target.view, None);
    }
}

impl Buffer {
    fn empty() -> Self {
        Buffer {
            raw: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            mapped: 0,
            capacity: 0,
        }
    }

    unsafe fn destroy(self, raw: &ash::Device) {
        if self.capacity > 0 {
            // SAFETY: the caller's contract: no submitted draw reads the buffer any more.
            unsafe {
                raw.destroy_buffer(self.raw, None);
                raw.free_memory(self.memory, None);
            }
        }
    }
}

/// Grows `buffer` to hold `needed` bytes; only while no submitted draw still reads it.
unsafe fn ensure_capacity(
    raw: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    buffer: &mut Buffer,
    needed: u64,
    usage: vk::BufferUsageFlags,
) -> Result<(), vk::Result> {
    if needed <= buffer.capacity {
        return Ok(());
    }
    let capacity = needed.max(MIN_BUFFER_SIZE).next_power_of_two();
    // SAFETY: the replaced buffer is no longer read, per the caller's contract.
    unsafe {
        let new_raw = raw.create_buffer(
            &vk::BufferCreateInfo::default().size(capacity).usage(usage),
            None,
        )?;
        let requirements = raw.get_buffer_memory_requirements(new_raw);
        let Some(type_index) = memory_type(
            memory,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) else {
            raw.destroy_buffer(new_raw, None);
            return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
        };
        let new_memory = raw.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(type_index),
            None,
        )?;
        raw.bind_buffer_memory(new_raw, new_memory, 0)?;
        let mapped = raw.map_memory(new_memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())?;
        std::mem::replace(
            buffer,
            Buffer {
                raw: new_raw,
                memory: new_memory,
                mapped: mapped as usize,
                capacity,
            },
        )
        .destroy(raw);
    }
    Ok(())
}

fn memory_type(
    memory: &vk::PhysicalDeviceMemoryProperties,
    allowed: u32,
    wanted: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..memory.memory_type_count).find(|&i| {
        allowed & (1 << i) != 0
            && memory.memory_types[i as usize]
                .property_flags
                .contains(wanted)
    })
}

/// Records copying `atlas` into the target's texture when it holds another version. Returns the
/// version that is current once the recorded commands ran.
unsafe fn upload_atlas(
    raw: &ash::Device,
    memory: &vk::PhysicalDeviceMemoryProperties,
    sampler: vk::Sampler,
    target: &mut Target,
    atlas: &Atlas,
) -> Result<Option<u64>, vk::Result> {
    if target
        .atlas
        .as_ref()
        .is_some_and(|t| t.version == Some(atlas.version))
    {
        return Ok(None);
    }
    let size = atlas.image.size;
    // SAFETY: the caller waited for the target's previous draw and is recording `target.commands`.
    unsafe {
        let old_layout = match target.atlas.take_if(|t| t.size != size) {
            Some(stale) => {
                stale.destroy(raw);
                vk::ImageLayout::UNDEFINED
            }
            None if target.atlas.is_some() => vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            None => vk::ImageLayout::UNDEFINED,
        };
        if target.atlas.is_none() {
            let texture = AtlasTexture::create(raw, memory, size)?;
            let image_info = vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(texture.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            raw.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(target.descriptor_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&image_info))],
                &[],
            );
            target.atlas = Some(texture);
        }
        let texture = target.atlas.as_ref().unwrap();

        let bytes = (atlas.image.pixels.len() * 4) as u64;
        ensure_capacity(
            raw,
            memory,
            &mut target.staging,
            bytes,
            vk::BufferUsageFlags::TRANSFER_SRC,
        )?;
        let staging = std::slice::from_raw_parts_mut(
            target.staging.mapped as *mut [u8; 4],
            atlas.image.pixels.len(),
        );
        for (dst, src) in staging.iter_mut().zip(&atlas.image.pixels) {
            *dst = src.to_array();
        }

        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        raw.cmd_pipeline_barrier(
            target.commands,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(texture.image)
                .subresource_range(range)],
        );
        raw.cmd_copy_buffer_to_image(
            target.commands,
            target.staging.raw,
            texture.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: size[0] as u32,
                    height: size[1] as u32,
                    depth: 1,
                })],
        );
        raw.cmd_pipeline_barrier(
            target.commands,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(texture.image)
                .subresource_range(range)],
        );
    }
    Ok(Some(atlas.version))
}

impl AtlasTexture {
    unsafe fn create(
        raw: &ash::Device,
        memory: &vk::PhysicalDeviceMemoryProperties,
        size: [usize; 2],
    ) -> Result<Self, vk::Result> {
        // UNORM: the shader multiplies the texels as the sRGB bytes epaint wrote.
        let format = vk::Format::R8G8B8A8_UNORM;
        // SAFETY: plain object creation on a live device; every create info outlives its call.
        unsafe {
            let image = raw.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: size[0] as u32,
                        height: size[1] as u32,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?;
            let requirements = raw.get_image_memory_requirements(image);
            let type_index = memory_type(
                memory,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .or_else(|| {
                memory_type(
                    memory,
                    requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::empty(),
                )
            })
            .expect("an image's memory requirements allow at least one type");
            let memory = match raw.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(type_index),
                None,
            ) {
                Ok(memory) => memory,
                Err(e) => {
                    raw.destroy_image(image, None);
                    return Err(e);
                }
            };
            let view = raw.bind_image_memory(image, memory, 0).and_then(|()| {
                raw.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                )
            });
            let view = match view {
                Ok(view) => view,
                Err(e) => {
                    raw.destroy_image(image, None);
                    raw.free_memory(memory, None);
                    return Err(e);
                }
            };
            Ok(AtlasTexture {
                image,
                memory,
                view,
                size,
                version: None,
            })
        }
    }

    unsafe fn destroy(self, raw: &ash::Device) {
        // SAFETY: the caller's contract: no submitted draw samples the texture any more.
        unsafe {
            raw.destroy_image_view(self.view, None);
            raw.destroy_image(self.image, None);
            raw.free_memory(self.memory, None);
        }
    }
}
