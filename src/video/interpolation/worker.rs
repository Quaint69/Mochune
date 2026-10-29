//! Bounded GPU-only prepare-ahead. The UI never waits for unfinished GPU work.
use super::*;
use anyhow::Context;
use gst_gl::prelude::*;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

const INPUTS: usize = 3;
const SLOTS: usize = 8;
type Notify = Arc<Mutex<Box<dyn Fn() + Send>>>;
#[derive(Clone)]
pub(crate) struct Input {
    pub info: gst_video::VideoInfo,
    pub buffer: gst::Buffer,
    pub due: Instant,
}
pub(crate) struct Frame {
    pub slot: usize,
    pub texture: u32,
    pub fence: usize,
    pub pts: u64,
    pub due: Instant,
    pub synthetic: bool,
}
#[derive(Default)]
struct Queues {
    input: VecDeque<Input>,
    ready: VecDeque<Frame>,
    recycled: VecDeque<(usize, usize)>,
    error: Option<String>,
}
pub(crate) struct Shared {
    queues: Mutex<Queues>,
    wake: Condvar,
    stop: AtomicBool,
    owner_released: AtomicBool,
    pub auto_fallback: AtomicBool,
    notify: Notify,
}
impl Shared {
    pub fn submit(&self, input: Input) {
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        let mut q = self.queues.lock().unwrap();
        if q.input.len() == INPUTS {
            q.input.pop_front();
        }
        q.input.push_back(input);
        drop(q);
        self.wake.notify_one();
    }
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_all();
    }
    fn notify(&self) {
        (self.notify.lock().unwrap())();
    }
}
pub(crate) struct Worker {
    pub shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn new(
        parent: &gst_gl::GLContext,
        first: Input,
        mode: i32,
        auto: bool,
        notify: Notify,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            owner_released: AtomicBool::new(false),
            auto_fallback: AtomicBool::new(auto),
            notify,
        });
        shared.submit(first);
        let state = shared.clone();
        let parent = parent.clone();
        let handle = thread::Builder::new()
            .name("interpolation".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let context = gst_gl::GLContext::new(&parent.display());
                    context
                        .create(Some(&parent))
                        .context("Cannot create shared interpolation GPU context")?;
                    let mut result = Ok(());
                    context.thread_add(|context| {
                        result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run(context, &state, mode)
                        }))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Interpolation worker failed")));
                    });
                    result
                })();
                if let Err(error) = result {
                    if !state.stop.load(Ordering::Acquire) {
                        state.queues.lock().unwrap().error = Some(format!("{error:#}"));
                        state.notify();
                    }
                }
            })?;
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }
    pub fn error(&self) -> Option<String> {
        self.shared.queues.lock().unwrap().error.take()
    }
    pub fn delay(&self) -> Duration {
        self.shared
            .queues
            .lock()
            .unwrap()
            .ready
            .front()
            .map_or(Duration::from_millis(16), |f| {
                f.due
                    .saturating_duration_since(Instant::now())
                    .saturating_sub(policy::DISPLAY_LEAD)
                    .max(Duration::from_millis(1))
            })
    }
    /// Only completed, due frames may cross into the UI. Missed deadlines skip
    /// old outputs rather than bursting through them or delaying the audio clock.
    pub fn take_ready(&self, gl: &glow::Context) -> Result<Option<Frame>> {
        let mut q = self.shared.queues.lock().unwrap();
        let mut chosen = None;
        while let Some(front) = q.ready.front() {
            if front.due > Instant::now() + policy::DISPLAY_LEAD {
                break;
            }
            let status = unsafe { gl.client_wait_sync(fence(front.fence), 0, 0) };
            anyhow::ensure!(
                status != glow::WAIT_FAILED,
                "Interpolation output fence failed"
            );
            if status != glow::ALREADY_SIGNALED && status != glow::CONDITION_SATISFIED {
                break;
            }
            let mut frame = q.ready.pop_front().unwrap();
            unsafe {
                gl.delete_sync(fence(frame.fence));
            }
            frame.fence = 0;
            if let Some(old) = chosen.replace(frame) {
                q.recycled.push_back((old.slot, 0));
            }
        }
        drop(q);
        self.shared.wake.notify_one();
        Ok(chosen)
    }
    /// The producer cannot overwrite an output until the UI's GPU copy finishes.
    pub fn recycle(&self, gl: &glow::Context, frame: Frame) -> Result<()> {
        let sync = unsafe { gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) }
            .map_err(anyhow::Error::msg)?;
        unsafe {
            gl.flush();
        }
        self.shared
            .queues
            .lock()
            .unwrap()
            .recycled
            .push_back((frame.slot, sync.0 as usize));
        self.shared.wake.notify_one();
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.owner_released.store(true, Ordering::Release);
        self.shared.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // All shared textures/fences have now been destroyed in their context.
        BYTES.store(0, Ordering::Relaxed);
        inactive();
    }
}
fn fence(raw: usize) -> glow::NativeFence {
    glow::NativeFence(raw as *mut _)
}
fn tex(raw: u32) -> glow::NativeTexture {
    glow::NativeTexture(std::num::NonZeroU32::new(raw).unwrap())
}
fn next_input(state: &Shared) -> Option<Input> {
    let q = state.queues.lock().unwrap();
    let mut q = state
        .wake
        .wait_while(q, |q| {
            q.input.is_empty() && !state.stop.load(Ordering::Acquire)
        })
        .unwrap();
    if state.stop.load(Ordering::Acquire) {
        None
    } else {
        q.input.pop_front()
    }
}
fn wait(state: &Shared) {
    let q = state.queues.lock().unwrap();
    let _ = state
        .wake
        .wait_timeout(q, Duration::from_millis(2))
        .unwrap();
}
fn map(
    context: &gst_gl::GLContext,
    input: &Input,
) -> Result<gst_gl::GLVideoFrame<gst_gl::gl_video_frame::Readable>> {
    input
        .buffer
        .meta::<gst_gl::GLSyncMeta>()
        .context("Missing decoded GPU synchronization")?
        .wait(context);
    gst_gl::GLVideoFrame::from_buffer_readable(input.buffer.clone(), &input.info)
        .map_err(|_| anyhow::anyhow!("Cannot import interpolation GPU frame"))
}
fn run(context: &gst_gl::GLContext, state: &Shared, mode: i32) -> Result<()> {
    let gl = unsafe {
        glow::Context::from_loader_function(|name| context.proc_address(name) as *const _)
    };
    let Some(first) = next_input(state) else {
        return Ok(());
    };
    let period = policy::interval(
        first.info.width(),
        first.info.height(),
        first.info.fps().numer(),
        first.info.fps().denom(),
    )
    .context("Unsupported interpolation source")?;
    let mut gpu = Gpu::new(&gl, first.info.width(), first.info.height())?;
    let mut textures = Vec::new();
    let mut fbos = Vec::new();
    let mut free = VecDeque::new();
    // Retain storage through GPU completion, including early error returns.
    let mut source_leases = vec![first.buffer.clone()];
    let result = (|| -> Result<()> {
        unsafe {
            for slot in 0..SLOTS {
                let texture = gl.create_texture().map_err(anyhow::Error::msg)?;
                textures.push(texture);
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::LINEAR as i32);
                }
                for name in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::CLAMP_TO_EDGE as i32);
                }
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    gpu.size.0 as i32,
                    gpu.size.1 as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                let fbo = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
                fbos.push(fbo);
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::TEXTURE_2D,
                    Some(texture),
                    0,
                );
                anyhow::ensure!(
                    gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER)
                        == glow::FRAMEBUFFER_COMPLETE,
                    "Interpolation output unavailable"
                );
                free.push_back(slot);
            }
        }
        BYTES.fetch_add(
            SLOTS as u64 * gpu.size.0 as u64 * gpu.size.1 as u64 * 4,
            Ordering::Relaxed,
        );
        crate::diagnostics::record(
            "interpolation_started",
            serde_json::json!({"backend":"shared GPU prepare-ahead", "width":gpu.size.0,"height":gpu.size.1,"source_fps":first.info.fps().numer() as f64 / first.info.fps().denom() as f64,"mode":mode,"lookahead_source_frames":policy::LOOKAHEAD_SOURCE_FRAMES,"output_slots":SLOTS}),
        );
        let read = unsafe { gl.create_framebuffer().map_err(anyhow::Error::msg)? };
        // Track this FBO with the others even on errors/cancellation.
        fbos.push(read);
        let mut previous = first;
        let mut a = map(context, &previous)?;
        loop {
            if state.stop.load(Ordering::Acquire) {
                break;
            }
            let Some(next) = next_input(state) else {
                break;
            };
            anyhow::ensure!(
                (next.info.width(), next.info.height()) == gpu.size
                    && next.info.fps() == previous.info.fps(),
                "Interpolation stream changed"
            );
            source_leases.push(next.buffer.clone());
            let b = map(context, &next)?;
            let p = previous
                .buffer
                .pts()
                .context("Missing interpolation timestamp")?
                .nseconds();
            let n = next
                .buffer
                .pts()
                .context("Missing interpolation timestamp")?
                .nseconds();
            if !policy::continuous(p, n, period) {
                source_leases.clear();
                source_leases.push(next.buffer.clone());
                previous = next;
                a = b;
                continue;
            }
            let origin = next
                .due
                .checked_sub(Duration::from_nanos(n - p))
                .unwrap_or(next.due);
            let plan = policy::plan(p, n, mode);
            let mut estimate = true;
            for tick in &plan.ticks[..plan.len] {
                let due = origin + Duration::from_nanos(tick.pts - p);
                if due + period < Instant::now() {
                    continue;
                }
                // Wait only on this worker. Neither a busy timing-query slot nor
                // a consumer texture fence may block the UI or grow the queue.
                let slot = loop {
                    if state.stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let retired: Vec<_> = state.queues.lock().unwrap().recycled.drain(..).collect();
                    let mut pending = Vec::new();
                    for (slot, sync) in retired {
                        let ready = if sync == 0 {
                            true
                        } else {
                            let status = unsafe { gl.client_wait_sync(fence(sync), 0, 0) };
                            anyhow::ensure!(
                                status != glow::WAIT_FAILED,
                                "Interpolation consumer fence failed"
                            );
                            status == glow::ALREADY_SIGNALED || status == glow::CONDITION_SATISFIED
                        };
                        if ready {
                            if sync != 0 {
                                unsafe {
                                    gl.delete_sync(fence(sync));
                                }
                            }
                            free.push_back(slot);
                        } else {
                            pending.push((slot, sync));
                        }
                    }
                    state.queues.lock().unwrap().recycled.extend(pending);
                    if gpu.work_ready(&gl) {
                        if let Some(slot) = free.pop_front() {
                            break slot;
                        }
                    }
                    wait(state);
                };
                let auto = state.auto_fallback.load(Ordering::Relaxed);
                if estimate || tick.fraction > 0. {
                    gpu.render(
                        &gl,
                        a.texture_id(0)?,
                        b.texture_id(0)?,
                        policy::output_period(mode, period),
                        tick.fraction,
                        estimate,
                        auto,
                        period,
                    )?;
                }
                estimate = false;
                let source = if tick.fraction > 0. {
                    gpu.texture()
                } else {
                    a.texture_id(0)?
                };
                let sync = unsafe {
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read));
                    gl.framebuffer_texture_2d(
                        glow::READ_FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(tex(source)),
                        0,
                    );
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(fbos[slot]));
                    gl.blit_framebuffer(
                        0,
                        0,
                        gpu.size.0 as i32,
                        gpu.size.1 as i32,
                        0,
                        0,
                        gpu.size.0 as i32,
                        gpu.size.1 as i32,
                        glow::COLOR_BUFFER_BIT,
                        glow::NEAREST,
                    );
                    let fence = gl
                        .fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0)
                        .map_err(anyhow::Error::msg)?;
                    gl.flush();
                    fence.0 as usize
                };
                let notify = {
                    let mut q = state.queues.lock().unwrap();
                    let empty = q.ready.is_empty();
                    q.ready.push_back(Frame {
                        slot,
                        texture: textures[slot].0.get(),
                        fence: sync,
                        pts: tick.pts,
                        due,
                        synthetic: tick.fraction > 0.,
                    });
                    empty
                };
                // Future results already have a presentation timer. Do not
                // redraw the UI for every frame prepared ahead of that timer.
                if notify {
                    state.notify();
                }
            }
            // A decoded texture belongs to an upstream pool. Do not return
            // its storage while GPU interpolation still reads it. This fence
            // is polled on the worker only; the UI remains completely async.
            let consumed = unsafe { gl.fence_sync(glow::SYNC_GPU_COMMANDS_COMPLETE, 0) }
                .map_err(anyhow::Error::msg)?;
            unsafe {
                gl.flush();
            }
            let started = Instant::now();
            let finished = (|| -> Result<()> {
                loop {
                    let status = unsafe { gl.client_wait_sync(consumed, 0, 0) };
                    anyhow::ensure!(
                        status != glow::WAIT_FAILED,
                        "Interpolation input fence failed"
                    );
                    if status == glow::ALREADY_SIGNALED || status == glow::CONDITION_SATISFIED {
                        break;
                    }
                    anyhow::ensure!(
                        started.elapsed() < Duration::from_millis(750),
                        "Interpolation GPU input stalled"
                    );
                    wait(state);
                }
                Ok(())
            })();
            unsafe {
                gl.delete_sync(consumed);
            }
            finished?;
            source_leases.clear();
            source_leases.push(next.buffer.clone());
            previous = next;
            a = b;
        }
        Ok(())
    })();
    if let Err(ref error) = result {
        if !state.stop.load(Ordering::Acquire) {
            state.queues.lock().unwrap().error = Some(format!("{error:#}"));
            state.notify();
        }
    }
    // Flush can invalidate inputs from the streaming thread. Keep shared GL
    // objects alive until the UI owner reaches its own safe teardown point.
    while !state.owner_released.load(Ordering::Acquire) {
        wait(state);
    }
    let teardown_started = Instant::now();
    let pending_consumers;
    unsafe {
        let mut q = state.queues.lock().unwrap();
        // The UI flushes these fences after copying our output. Finishing only
        // this context does not cover that other context's reads: join them to
        // this GPU queue before destroying shared textures. This blocks only
        // teardown, never normal frame presentation.
        pending_consumers = q.recycled.iter().filter(|(_, sync)| *sync != 0).count();
        for &(_, sync) in &q.recycled {
            if sync != 0 {
                gl.wait_sync(fence(sync), 0, glow::TIMEOUT_IGNORED);
            }
        }
        gl.finish();
        source_leases.clear();
        for frame in q.ready.drain(..) {
            if frame.fence != 0 {
                gl.delete_sync(fence(frame.fence));
            }
        }
        for (_, sync) in q.recycled.drain(..) {
            if sync != 0 {
                gl.delete_sync(fence(sync));
            }
        }
        q.input.clear();
        for f in fbos {
            gl.delete_framebuffer(f);
        }
        for t in textures {
            gl.delete_texture(t);
        }
    }
    gpu.release(&gl);
    crate::diagnostics::record(
        "interpolation_stopped",
        serde_json::json!({"consumer_fences":pending_consumers,"teardown_ms":teardown_started.elapsed().as_secs_f64()*1000.0}),
    );
    result
}
