//! Windows GPU-only video presentation: D3D12 conversion, shared NT resource,
//! WGPU GPU copy, then Slint. Never maps decoded pixels or uses a download element.
use super::*;
use gst::glib::translate::from_glib_full;
use slint::wgpu_30::wgpu;
use std::{ffi::c_void, ptr};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Graphics::{
            Direct3D12::*,
            Dxgi::{
                Common::DXGI_FORMAT_R8G8B8A8_UNORM, CreateDXGIFactory1,
                DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
                DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter1, IDXGIAdapter3, IDXGIFactory4,
            },
        },
    },
    core::Interface,
};

// The installed SDK is 1.26.11. These stable C APIs were introduced in 1.26.
// GstMemory is opaque here: access goes through the SDK's exported functions.
#[link(name = "gstd3d12-1.0")]
unsafe extern "C" {
    fn gst_d3d12_device_new_for_adapter_luid(luid: i64) -> *mut gst::ffi::GstObject;
    fn gst_d3d12_context_new(device: *mut gst::ffi::GstObject) -> *mut gst::ffi::GstContext;
    fn gst_is_d3d12_memory(memory: *mut gst::ffi::GstMemory) -> i32;
    fn gst_d3d12_memory_get_nt_handle(memory: *mut gst::ffi::GstMemory, handle: *mut HANDLE)
    -> i32;
    fn gst_d3d12_memory_get_fence(
        memory: *mut gst::ffi::GstMemory,
        fence: *mut *mut c_void,
        value: *mut u64,
    ) -> i32;
    fn gst_d3d12_memory_get_subresource_index(
        memory: *mut gst::ffi::GstMemory,
        plane: u32,
        index: *mut u32,
    ) -> i32;
}
static ADAPTER_INFO: Mutex<String> = Mutex::new(String::new());
static CAPABILITIES: Mutex<String> = Mutex::new(String::new());
pub(super) fn adapter_info() -> String {
    ADAPTER_INFO.lock().unwrap().clone()
}
pub(super) fn capabilities() -> String {
    CAPABILITIES.lock().unwrap().clone()
}
static DRIVER_MEMORY: Mutex<Option<serde_json::Value>> = Mutex::new(None);
static SHARED_CONTEXT_CREATED: AtomicBool = AtomicBool::new(false);
static ALLOCATOR_AVAILABLE: AtomicBool = AtomicBool::new(false);
static ALLOCATED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESERVED_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SUBMITTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COMPLETED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SUPERSEDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRODUCER_PENDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CONSUMER_PENDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SURFACE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static IN_FLIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(super) fn stats() -> serde_json::Value {
    serde_json::json!({"backend":"D3D12/WGPU","cpu_readback_allowed":false,"submitted":SUBMITTED.load(Ordering::Relaxed),"completed":COMPLETED.load(Ordering::Relaxed),"superseded":SUPERSEDED.load(Ordering::Relaxed),"producer_pending_polls":PRODUCER_PENDING.load(Ordering::Relaxed),"consumer_pending_polls":CONSUMER_PENDING.load(Ordering::Relaxed),"in_flight":IN_FLIGHT.load(Ordering::Relaxed),"surface_bytes":SURFACE_BYTES.load(Ordering::Relaxed),"shared_decoder_context_created":SHARED_CONTEXT_CREATED.load(Ordering::Relaxed),"driver_gpu_memory":DRIVER_MEMORY.lock().unwrap().clone(),"wgpu_allocator": if ALLOCATOR_AVAILABLE.load(Ordering::Relaxed) {serde_json::json!({"allocated_bytes":ALLOCATED_BYTES.load(Ordering::Relaxed),"reserved_bytes":RESERVED_BYTES.load(Ordering::Relaxed)})} else {serde_json::Value::Null}})
}
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    native: ID3D12Device,
    fence: ID3D12Fence,
    serial: u64,
    last_memory_sample: Option<std::time::Instant>,
    memory_adapter: Option<IDXGIAdapter3>,
    // Exactly one GPU copy may be in flight. Retain the source sample until the
    // consumer fence completes, so GStreamer cannot recycle it under the GPU.
    in_flight: Option<(u64, gst::Buffer, wgpu::Texture)>,
    output: Option<(wgpu::Texture, slint::Image)>,
}
impl Gpu {
    fn reap(&mut self) -> Result<bool> {
        let _ = self.device.poll(wgpu::PollType::Poll);
        if self
            .last_memory_sample
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(5))
        {
            self.last_memory_sample = Some(std::time::Instant::now());
            if let Some(report) = self.device.generate_allocator_report() {
                ALLOCATED_BYTES.store(report.total_allocated_bytes, Ordering::Relaxed);
                RESERVED_BYTES.store(report.total_reserved_bytes, Ordering::Relaxed);
                ALLOCATOR_AVAILABLE.store(true, Ordering::Relaxed);
            } else {
                ALLOCATOR_AVAILABLE.store(false, Ordering::Relaxed);
            }
            if let Some(adapter) = &self.memory_adapter {
                let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                let mut nonlocal = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                // Driver accounting includes both UI and external decoder resources
                // on this adapter. Segment-local memory is not always dedicated VRAM.
                let local_ok = unsafe {
                    adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local)
                }
                .is_ok();
                let nonlocal_ok = unsafe {
                    adapter.QueryVideoMemoryInfo(
                        0,
                        DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
                        &mut nonlocal,
                    )
                }
                .is_ok();
                *DRIVER_MEMORY.lock().unwrap() = Some(serde_json::json!({
                    "local_usage_bytes":local_ok.then_some(local.CurrentUsage),
                    "nonlocal_usage_bytes":nonlocal_ok.then_some(nonlocal.CurrentUsage),
                    "local_budget_bytes":local_ok.then_some(local.Budget),
                    "nonlocal_budget_bytes":nonlocal_ok.then_some(nonlocal.Budget)
                }));
            }
        }
        let completed = unsafe { self.fence.GetCompletedValue() };
        anyhow::ensure!(
            completed != u64::MAX,
            "Direct3D device was removed during video presentation"
        );
        if self
            .in_flight
            .as_ref()
            .is_some_and(|(value, _, _)| completed < *value)
        {
            return Ok(false);
        }
        if self.in_flight.take().is_some() {
            COMPLETED.fetch_add(1, Ordering::Relaxed);
            IN_FLIGHT.store(0, Ordering::Relaxed);
        }
        Ok(true)
    }
    fn copy(
        &mut self,
        info: &gst_video::VideoInfo,
        buffer: gst::Buffer,
    ) -> Result<(slint::Image, bool, u64)> {
        anyhow::ensure!(
            info.format() == gst_video::VideoFormat::Rgba,
            "D3D12 presentation requires GPU-converted RGBA"
        );
        let memory = buffer.peek_memory(0).as_ptr() as *mut gst::ffi::GstMemory;
        let mut handle = HANDLE::default();
        let mut subresource = 0;
        // SDK owns the exported handle. OpenSharedHandle takes a COM reference;
        // never CloseHandle the handle borrowed from GstD3D12Memory.
        anyhow::ensure!(
            unsafe { gst_d3d12_memory_get_nt_handle(memory, &mut handle) } != 0,
            "The GPU frame is not shareable; no CPU-readback fallback was used"
        );
        anyhow::ensure!(
            unsafe { gst_d3d12_memory_get_subresource_index(memory, 0, &mut subresource) } != 0
                && subresource == 0,
            "Unexpected D3D12 texture-array subresource"
        );
        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            self.native.OpenSharedHandle(handle, &mut resource)?;
        }
        let resource =
            resource.context("Opening the shared D3D12 video resource returned no texture")?;
        let desc = unsafe { resource.GetDesc() };
        anyhow::ensure!(
            desc.Format == DXGI_FORMAT_R8G8B8A8_UNORM
                && desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
                && desc.DepthOrArraySize == 1
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1,
            "Unsupported shared D3D12 resource layout"
        );
        anyhow::ensure!(
            desc.Flags & D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS != D3D12_RESOURCE_FLAG_NONE,
            "The converted GPU resource must allow simultaneous access / COMMON-state decay"
        );
        anyhow::ensure!(
            desc.Width >= u64::from(info.width())
                && desc.Width <= u64::from(u32::MAX)
                && desc.Height >= info.height(),
            "Shared GPU texture is smaller than the visible video"
        );
        let source_size = wgpu::Extent3d {
            width: desc.Width as u32,
            height: desc.Height,
            depth_or_array_layers: 1,
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let source_desc = wgpu::TextureDescriptor {
            label: Some("Mochune shared decoder frame"),
            size: source_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        anyhow::ensure!(
            source_size.width <= self.device.limits().max_texture_dimension_2d
                && source_size.height <= self.device.limits().max_texture_dimension_2d,
            "Shared video dimensions exceed this GPU renderer limit"
        );
        // Safety: OpenSharedHandle imports onto this exact WGPU D3D12 device.
        // The producer fence was checked before this call. Its simultaneous-
        // access resource has decayed to COMMON after producer submission.
        let hal = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                format,
                wgpu::TextureDimension::D2,
                source_size,
                1,
                1,
            )
        };
        let source = unsafe {
            self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                hal,
                &source_desc,
                wgpu::TextureUses::PRESENT,
            )
        };
        anyhow::ensure!(
            info.width() <= self.device.limits().max_texture_dimension_2d
                && info.height() <= self.device.limits().max_texture_dimension_2d,
            "Video dimensions exceed this GPU renderer limit"
        );
        let size = wgpu::Extent3d {
            width: info.width(),
            height: info.height(),
            depth_or_array_layers: 1,
        };
        let changed = self
            .output
            .as_ref()
            .is_none_or(|(texture, _)| texture.size() != size);
        if changed {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Mochune persistent video surface"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let image = slint::Image::try_from(texture.clone())
                .map_err(|e| anyhow::anyhow!("Slint GPU texture import: {e}"))?;
            self.output = Some((texture, image));
            SURFACE_BYTES.store(
                u64::from(size.width) * u64::from(size.height) * 4,
                Ordering::Relaxed,
            );
            crate::diagnostics::record(
                "gpu_surface",
                serde_json::json!({"backend":"D3D12/WGPU","size":[size.width,size.height],"cpu_pixel_copies":0,"gpu_copies_per_frame":1,"max_in_flight":1}),
            );
        }
        let (output, image) = self.output.as_ref().unwrap();
        let image = image.clone();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Mochune GPU-only video copy"),
            });
        encoder.copy_texture_to_texture(source.as_image_copy(), output.as_image_copy(), size);
        // Return the shared texture to COMMON before releasing it to its producer.
        // WGPU knows this transition; no out-of-band barriers or stale tracking.
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &source,
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        let pts = buffer.pts().map_or(0, |v| v.nseconds());
        self.serial = self
            .serial
            .checked_add(1)
            .context("GPU fence serial exhausted")?;
        self.queue.submit([encoder.finish()]);
        // Retain BEFORE signaling, including error paths after GPU submission.
        self.in_flight = Some((self.serial, buffer, source));
        SUBMITTED.fetch_add(1, Ordering::Relaxed);
        IN_FLIGHT.store(1, Ordering::Relaxed);
        let native_queue = unsafe { self.queue.as_hal::<wgpu::hal::api::Dx12>() }
            .context("Slint queue is not Direct3D12")?;
        unsafe {
            native_queue.as_raw().Signal(&self.fence, self.serial)?;
        }
        Ok((image, changed, pts))
    }
    fn release(&mut self) {
        // Only teardown can wait. Normal rendering merely polls fences and drops
        // superseded frames, never stalls the UI for decode or pixel transfers.
        let finished = self
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(2)),
            })
            .is_ok();
        if finished && self.in_flight.take().is_some() {
            COMPLETED.fetch_add(1, Ordering::Relaxed);
            IN_FLIGHT.store(0, Ordering::Relaxed);
        }
        if !finished && !self.reap().unwrap_or(true) {
            // A hung driver must not recycle a buffer still referenced by a GPU.
            // Quarantine at most one sample until the fence completes/device dies.
            if let Some((value, buffer, texture)) = self.in_flight.take() {
                let fence = self.fence.clone();
                std::thread::spawn(move || {
                    while unsafe { fence.GetCompletedValue() } < value {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    drop((buffer, texture));
                    COMPLETED.fetch_add(1, Ordering::Relaxed);
                    IN_FLIGHT.store(0, Ordering::Relaxed);
                });
            }
        }
        self.output = None;
        SURFACE_BYTES.store(0, Ordering::Relaxed);
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) struct TextureSink {
    pub(super) glsink: gst::Element,
    appsink: gst_app::AppSink,
    pub(super) next: Arc<Mutex<FrameMailbox>>,
    pub(super) redraw_pending: Option<Arc<AtomicBool>>,
    notify: Option<Arc<Mutex<Box<dyn Fn() + Send>>>>,
    pending: Option<(gst_video::VideoInfo, gst::Buffer)>,
    epoch: u64,
    gpu: Option<Gpu>,
    gst_context: Arc<Mutex<Option<gst::Context>>>,
}
impl TextureSink {
    pub(super) fn new() -> Result<Self> {
        anyhow::ensure!(
            gst::version() >= (1, 26, 0, 0),
            "Windows native GPU playback requires GStreamer 1.26 or newer"
        );
        let caps = gst_video::VideoCapsBuilder::new()
            .features(["memory:D3D12Memory"])
            .format(gst_video::VideoFormat::Rgba)
            .build();
        let appsink = gst_app::AppSink::builder()
            .caps(&caps)
            .enable_last_sample(false)
            .max_buffers(1)
            .drop(true)
            .async_(true)
            .build();
        appsink.set_property("qos", true);
        appsink.set_property("max-lateness", 50_000_000i64);
        let convert = gst::ElementFactory::make("d3d12convert")
            .build()
            .context("GStreamer D3D12 GPU converter is unavailable (requires GStreamer 1.26+)")?;
        // Always convert YUV to RGBA in GPU memory; no download/upload element.
        let bin = gst::Bin::new();
        bin.add_many([&convert, appsink.upcast_ref()])?;
        convert.link(&appsink)?;
        bin.add_pad(&gst::GhostPad::with_target(
            &convert.static_pad("sink").unwrap(),
        )?)?;
        let next = Arc::new(Mutex::new(FrameMailbox::default()));
        let frames = next.clone();
        appsink.static_pad("sink").unwrap().add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::EVENT_FLUSH,
            move |_, info| {
                if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref() {
                    match event.view() {
                        gst::EventView::FlushStart(_) => frames.lock().unwrap().clear(true),
                        gst::EventView::FlushStop(_) => frames.lock().unwrap().flushing = false,
                        _ => {}
                    }
                }
                gst::PadProbeReturn::Ok
            },
        );
        Ok(Self {
            glsink: bin.upcast(),
            appsink,
            next,
            redraw_pending: None,
            notify: None,
            pending: None,
            epoch: 0,
            gpu: None,
            gst_context: Arc::new(Mutex::new(None)),
        })
    }
    pub(super) fn connect(
        &mut self,
        api: &slint::GraphicsAPI<'_>,
        bus: &gst::Bus,
        sender: mpsc::Sender<gst::Message>,
        notify: Box<dyn Fn() + Send>,
    ) -> Result<()> {
        let slint::GraphicsAPI::WGPU30 { device, queue, .. } = api else {
            bail!("Windows video requires Slint's Direct3D12/WGPU renderer");
        };
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
            .context("Slint did not select the Direct3D12 backend")?;
        let native = hal.raw_device().clone();
        let luid = unsafe { native.GetAdapterLuid() };
        let luid_bits = ((luid.HighPart as i64) << 32) | i64::from(luid.LowPart);
        let mut decoders = Vec::new();
        for factory in gst::ElementFactory::factories_with_type(
            gst::ElementFactoryType::DECODER,
            gst::Rank::NONE,
        ) {
            if factory.name().starts_with("d3d11") {
                factory.set_rank(gst::Rank::NONE);
            }
            if !factory.name().starts_with("d3d12") {
                continue;
            }
            let matches = factory.create().build().ok().is_some_and(|element| {
                element.find_property("adapter-luid").is_some()
                    && element.property::<i64>("adapter-luid") == luid_bits
            });
            factory.set_rank(if matches {
                gst::Rank::from(356)
            } else {
                gst::Rank::NONE
            });
            if matches {
                decoders.push(factory.name().to_string());
            }
        }
        // Read the UI adapter description without eagerly creating a second
        // D3D12 device/queue set for audio-only sessions.
        let (description, memory_adapter) =
            (|| -> windows::core::Result<(String, Option<IDXGIAdapter3>)> {
                let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1()? };
                let adapter: IDXGIAdapter1 = unsafe { factory.EnumAdapterByLuid(luid)? };
                let desc = unsafe { adapter.GetDesc1()? };
                let len = desc
                    .Description
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(desc.Description.len());
                Ok((
                    String::from_utf16_lossy(&desc.Description[..len]),
                    adapter.cast().ok(),
                ))
            })()
            .unwrap_or_else(|_| (format!("adapter LUID {luid_bits}"), None));
        *ADAPTER_INFO.lock().unwrap() = format!("Direct3D 12 / WGPU · {description}");
        *CAPABILITIES.lock().unwrap() = if decoders.is_empty() {
            "No D3D12 hardware video decoders advertised on the UI GPU".to_owned()
        } else {
            format!(
                "UI GPU advertises: {} (playback verifies use)",
                decoders.join(", ")
            )
        };
        crate::diagnostics::record(
            "gpu_decoders",
            serde_json::json!({"adapter_luid":luid_bits,"matching_decoders":decoders,"software_video_fallback":false}),
        );

        let contexts = self.gst_context.clone();
        bus.set_sync_handler(move |_, message| {
            if let gst::MessageView::NeedContext(need) = message.view() {
                if need.context_type() == "gst.d3d12.device.handle"
                    && let Some(element) = message
                        .src()
                        .and_then(|src| src.downcast_ref::<gst::Element>())
                {
                    let mut stored = contexts.lock().unwrap();
                    if stored.is_none() {
                        let ptr = unsafe { gst_d3d12_device_new_for_adapter_luid(luid_bits) };
                        if ptr.is_null() {
                            gst::element_error!(element, gst::CoreError::Failed, ("Cannot create the D3D12 decoder device on the UI GPU"));
                            return gst::BusSyncReply::Drop;
                        }
                        let device: gst::Object = unsafe { from_glib_full(ptr) };
                        let context: gst::Context = unsafe { from_glib_full(gst_d3d12_context_new(device.as_ptr())) };
                        *stored = Some(context);
                        SHARED_CONTEXT_CREATED.store(true, Ordering::Relaxed);
                        crate::diagnostics::record("decoder_device_created", serde_json::json!({"adapter_luid":luid_bits,"process_memory":crate::diagnostics::process_memory()}));
                    }
                    // Release our mutex before setting a context: that can trigger
                    // additional context requests from other pipeline elements.
                    let context = stored.as_ref().unwrap().clone();
                    drop(stored);
                    element.set_context(&context);
                }
            } else {
                let _ = sender.send(message.to_owned());
            }
            gst::BusSyncReply::Drop
        });
        let error_sink = self.glsink.downgrade();
        device.on_uncaptured_error(Arc::new(move |error| {
            let detail = format!("D3D12/WGPU error: {error}");
            crate::diagnostics::record("gpu_error", serde_json::json!({"detail":detail}));
            if let Some(sink) = error_sink.upgrade() {
                gst::element_error!(sink, gst::CoreError::Failed, ("{detail}"));
            }
        }));
        let lost_sink = self.glsink.downgrade();
        device.set_device_lost_callback(move |reason, message| {
            if reason == wgpu::DeviceLostReason::Destroyed {
                return;
            }
            let detail = format!("D3D12 device lost ({reason:?}): {message}");
            crate::diagnostics::record("gpu_device_lost", serde_json::json!({"detail":detail}));
            if let Some(sink) = lost_sink.upgrade() {
                gst::element_error!(sink, gst::CoreError::Failed, ("{detail}"));
            }
        });
        let fence: ID3D12Fence = unsafe { native.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        self.gpu = Some(Gpu {
            device: device.clone(),
            queue: queue.clone(),
            native,
            fence,
            serial: 0,
            last_memory_sample: None,
            memory_adapter,
            in_flight: None,
            output: None,
        });

        let notify = Arc::new(Mutex::new(notify));
        let next = self.next.clone();
        let preroll = next.clone();
        let preroll_notify = notify.clone();
        let frame_notify = notify.clone();
        self.appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_preroll(move |sink| receive(sink, true, &preroll, &preroll_notify))
                .new_sample(move |sink| receive(sink, false, &next, &frame_notify))
                .build(),
        );
        self.notify = Some(notify);
        crate::diagnostics::record(
            "graphics_backend",
            serde_json::json!({"api":"D3D12/WGPU","adapter_luid":luid_bits,"presentation":"shared GPU resource → GPU copy → Slint texture","cpu_pixel_copies":0,"non_sampler_binding_budget":device.limits().max_non_sampler_bindings}),
        );
        Ok(())
    }
    fn retry_redraw(&self) {
        if let Some(notify) = &self.notify {
            notify.lock().unwrap()();
        }
    }
    pub(super) fn next_image(&mut self) -> Result<Option<(slint::Image, bool, u64)>> {
        if let Some(q) = &self.redraw_pending {
            q.store(false, Ordering::Release);
        }
        let gpu = self
            .gpu
            .as_mut()
            .context("D3D12 renderer is not connected")?;
        if !gpu.reap()? {
            CONSUMER_PENDING.fetch_add(1, Ordering::Relaxed);
            self.retry_redraw();
            return Ok(None);
        }
        {
            let mut frames = self.next.lock().unwrap();
            if self.epoch != frames.epoch {
                self.pending = None;
                self.epoch = frames.epoch;
            }
            if frames.flushing {
                return Ok(None);
            }
            if let Some(newest) = frames.next.take() {
                self.pending = Some(newest);
            }
        }
        let Some((_, buffer)) = &self.pending else {
            return Ok(None);
        };
        let mut fence_ptr = ptr::null_mut();
        let mut value = 0;
        // Transfer-full COM ref from GStreamer. Only poll completion; never mutate
        // or signal a fence belonging to the producer.
        if unsafe {
            gst_d3d12_memory_get_fence(
                buffer.peek_memory(0).as_ptr() as *mut _,
                &mut fence_ptr,
                &mut value,
            )
        } != 0
            && !fence_ptr.is_null()
        {
            let fence = unsafe { ID3D12Fence::from_raw(fence_ptr) };
            let completed = unsafe { fence.GetCompletedValue() };
            anyhow::ensure!(completed != u64::MAX, "Decoder GPU device was removed");
            if completed < value {
                PRODUCER_PENDING.fetch_add(1, Ordering::Relaxed);
                self.retry_redraw();
                return Ok(None);
            }
        }
        let (info, buffer) = self.pending.take().unwrap();
        Ok(Some(gpu.copy(&info, buffer)?))
    }
    pub(super) fn clear(&mut self) {
        self.next.lock().unwrap().next.take();
        self.pending = None;
        if let Some(gpu) = self.gpu.as_mut() {
            let _ = gpu.reap();
            gpu.output = None;
            SURFACE_BYTES.store(0, Ordering::Relaxed);
        }
    }
    pub(super) fn release(&mut self) {
        self.clear();
        self.gpu = None;
        self.gst_context.lock().unwrap().take();
        SHARED_CONTEXT_CREATED.store(false, Ordering::Relaxed);
        *DRIVER_MEMORY.lock().unwrap() = None;
        ALLOCATOR_AVAILABLE.store(false, Ordering::Relaxed);
    }
}
fn receive(
    sink: &gst_app::AppSink,
    preroll: bool,
    mailbox: &Mutex<FrameMailbox>,
    notify: &Mutex<Box<dyn Fn() + Send>>,
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let epoch = mailbox.lock().unwrap().epoch;
    let sample = if preroll {
        sink.pull_preroll()
    } else {
        sink.pull_sample()
    }
    .map_err(|_| gst::FlowError::Flushing)?;
    let buffer = sample.buffer_owned().ok_or(gst::FlowError::Error)?;
    if buffer.n_memory() != 1
        || unsafe { gst_is_d3d12_memory(buffer.peek_memory(0).as_ptr() as *mut _) } == 0
    {
        return Err(gst::FlowError::NotNegotiated);
    }
    let info = sample
        .caps()
        .and_then(|caps| gst_video::VideoInfo::from_caps(caps).ok())
        .ok_or(gst::FlowError::NotNegotiated)?;
    {
        let mut frames = mailbox.lock().unwrap();
        if frames.flushing || frames.epoch != epoch {
            return Ok(gst::FlowSuccess::Ok);
        }
        if frames.next.is_some() {
            SUPERSEDED.fetch_add(1, Ordering::Relaxed);
        }
        frames.next = Some((info, buffer));
    }
    notify.lock().unwrap()();
    Ok(gst::FlowSuccess::Ok)
}
