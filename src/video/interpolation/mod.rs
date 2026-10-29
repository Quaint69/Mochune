//! Experimental 2×/60 fps motion-compensated presentation. Linux GL textures only:
//! no pixel readback, neural model, CPU decoding, or disk frame cache.
//! Motion is estimated and regularized once per bounded GPU pair, then reused.
//! Bounded coarse search images and motion maps preserve edges without retaining more frames.
use anyhow::{Result, bail};
use glow::HasContext;
use std::time::Duration;
pub(super) mod policy;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
static ACTIVE: AtomicBool = AtomicBool::new(false);
static GENERATED: AtomicU64 = AtomicU64::new(0);
static PRESENTED: AtomicU64 = AtomicU64::new(0);
static GPU_NS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
pub(crate) fn stats() -> serde_json::Value {
    serde_json::json!({"active":ACTIVE.load(Ordering::Relaxed),"generated":GENERATED.load(Ordering::Relaxed),"presented":PRESENTED.load(Ordering::Relaxed),"gpu_ms":GPU_NS.load(Ordering::Relaxed) as f64/1e6,"extra_texture_bytes":BYTES.load(Ordering::Relaxed)})
}
pub(super) fn inactive() {
    ACTIVE.store(false, Ordering::Relaxed);
}
pub(super) fn presented() {
    PRESENTED.fetch_add(1, Ordering::Relaxed);
}
pub(super) mod worker;
const VERTEX: &str = "out vec2 uv; void main(){vec2 p=vec2(float((gl_VertexID<<1)&2),float(gl_VertexID&2));uv=p;gl_Position=vec4(p*2.0-1.0,0,1);}";

// Every entry point is called with Slint's owning GL context current. Restore
// all touched state, including texture units, so the UI renderer sees no leaks.
struct State {
    program: u32,
    vao: u32,
    fbo: u32,
    viewport: [i32; 4],
    active: u32,
    textures: [u32; 7],
    flags: [bool; 5],
    mask: [bool; 4],
}
impl State {
    unsafe fn capture(gl: &glow::Context) -> Self {
        unsafe {
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mask = gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK);
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
            let mut textures = [0; 7];
            for (i, t) in textures.iter_mut().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                *t = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32;
            }
            gl.active_texture(active);
            Self {
                program: gl.get_parameter_i32(glow::CURRENT_PROGRAM) as u32,
                vao: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING) as u32,
                fbo: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32,
                viewport,
                active,
                textures,
                flags: [
                    gl.is_enabled(glow::BLEND),
                    gl.is_enabled(glow::SCISSOR_TEST),
                    gl.is_enabled(glow::DEPTH_TEST),
                    gl.is_enabled(glow::STENCIL_TEST),
                    gl.is_enabled(glow::CULL_FACE),
                ],
                mask,
            }
        }
    }
    unsafe fn restore(self, gl: &glow::Context) {
        unsafe {
            gl.use_program(std::num::NonZeroU32::new(self.program).map(glow::NativeProgram));
            gl.bind_vertex_array(std::num::NonZeroU32::new(self.vao).map(glow::NativeVertexArray));
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                std::num::NonZeroU32::new(self.fbo).map(glow::NativeFramebuffer),
            );
            gl.viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            for (i, t) in self.textures.into_iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    std::num::NonZeroU32::new(t).map(glow::NativeTexture),
                );
            }
            gl.active_texture(self.active);
            for (flag, on) in [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::DEPTH_TEST,
                glow::STENCIL_TEST,
                glow::CULL_FACE,
            ]
            .into_iter()
            .zip(self.flags)
            {
                if on {
                    gl.enable(flag);
                } else {
                    gl.disable(flag);
                }
            }
            gl.color_mask(self.mask[0], self.mask[1], self.mask[2], self.mask[3]);
        }
    }
}
unsafe fn program(gl: &glow::Context, fragment: &str) -> Result<glow::NativeProgram> {
    unsafe {
        let header = if gl.get_parameter_string(glow::VERSION).contains("OpenGL ES") {
            "#version 300 es\nprecision highp float;\nprecision highp int;\n"
        } else {
            "#version 330 core\n"
        };
        let p = gl.create_program().map_err(anyhow::Error::msg)?;
        let mut shaders = Vec::new();
        let result = (|| -> Result<()> {
            for (kind, body) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, fragment),
            ] {
                let s = gl.create_shader(kind).map_err(anyhow::Error::msg)?;
                shaders.push(s);
                gl.shader_source(s, &format!("{header}{body}"));
                gl.compile_shader(s);
                gl.attach_shader(p, s);
                if !gl.get_shader_compile_status(s) {
                    bail!("Interpolation shader: {}", gl.get_shader_info_log(s));
                }
            }
            gl.link_program(p);
            if !gl.get_program_link_status(p) {
                bail!("Interpolation shader link: {}", gl.get_program_info_log(p));
            }
            Ok(())
        })();
        for s in shaders {
            gl.detach_shader(p, s);
            gl.delete_shader(s);
        }
        if let Err(e) = result {
            gl.delete_program(p);
            return Err(e);
        }
        Ok(p)
    }
}
pub(super) struct Gpu {
    programs: Vec<glow::NativeProgram>,
    textures: Vec<glow::NativeTexture>,
    fbos: Vec<glow::NativeFramebuffer>,
    vao: Option<glow::NativeVertexArray>,
    queries: Vec<glow::NativeQuery>,
    pending: [bool; 3],
    cursor: usize,
    slow: [u8; 2],
    estimate_query: [bool; 3],
    query_budget: [Duration; 3],
    pub size: (u32, u32),
    grid: (u32, u32),
    pub gpu_ns: u64,
}
impl Gpu {
    pub fn new(gl: &glow::Context, width: u32, height: u32) -> Result<Self> {
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) }.to_ascii_lowercase();
        anyhow::ensure!(
            ![
                "llvmpipe",
                "softpipe",
                "swiftshader",
                "software rasterizer",
                "swrast"
            ]
            .iter()
            .any(|name| renderer.contains(name)),
            "Interpolation needs a hardware GPU renderer; native playback retained"
        );
        let version = unsafe { gl.get_parameter_string(glow::VERSION) };
        anyhow::ensure!(
            !version.contains("OpenGL ES")
                || gl
                    .supported_extensions()
                    .contains("GL_EXT_disjoint_timer_query"),
            "GPU timing unavailable; native playback retained"
        );
        let mut s = Self {
            programs: vec![],
            textures: vec![],
            fbos: vec![],
            vao: None,
            queries: vec![],
            pending: [false; 3],
            cursor: 0,
            slow: [0; 2],
            estimate_query: [false; 3],
            query_budget: [Duration::ZERO; 3],
            size: (width, height),
            grid: (width.div_ceil(8), height.div_ceil(8)),
            gpu_ns: 0,
        };
        unsafe {
            let state = State::capture(gl);
            let result = (|| -> Result<()> {
                s.programs.push(program(gl, include_str!("motion.frag"))?);
                s.programs.push(program(gl, include_str!("refine.frag"))?);
                s.programs.push(program(gl, include_str!("seed.frag"))?);
                s.programs.push(program(gl, include_str!("warp.frag"))?);
                s.programs.push(program(gl, include_str!("preview.frag"))?);
                s.programs
                    .push(program(gl, include_str!("consensus.frag"))?);
                gl.active_texture(glow::TEXTURE0);
                s.vao = Some(gl.create_vertex_array().map_err(anyhow::Error::msg)?);
                for (i, (w, h)) in [s.grid, s.size, s.grid, s.grid, s.grid, s.grid, (1, 1)]
                    .into_iter()
                    .enumerate()
                {
                    let t = gl.create_texture().map_err(anyhow::Error::msg)?;
                    s.textures.push(t);
                    gl.bind_texture(glow::TEXTURE_2D, Some(t));
                    for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                        gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::LINEAR as i32);
                    }
                    for name in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                        gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::CLAMP_TO_EDGE as i32);
                    }
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        if i == 1 || i == 4 || i == 5 {
                            glow::RGBA8
                        } else {
                            glow::RGBA16F
                        } as i32,
                        w as i32,
                        h as i32,
                        0,
                        glow::RGBA,
                        if i == 1 || i == 4 || i == 5 {
                            glow::UNSIGNED_BYTE
                        } else {
                            glow::HALF_FLOAT
                        },
                        glow::PixelUnpackData::Slice(None),
                    );
                    let f = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
                    s.fbos.push(f);
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(f));
                    gl.framebuffer_texture_2d(
                        glow::DRAW_FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(t),
                        0,
                    );
                    anyhow::ensure!(
                        gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER)
                            == glow::FRAMEBUFFER_COMPLETE,
                        "Interpolation render target unavailable"
                    );
                }
                // Asynchronous GPU timing: never wait for a query or read video pixels.
                for _ in 0..3 {
                    s.queries
                        .push(gl.create_query().map_err(anyhow::Error::msg)?);
                }
                Ok(())
            })();
            state.restore(gl);
            if let Err(e) = result {
                s.release(gl);
                return Err(e);
            }
        }
        BYTES.store(
            u64::from(width) * u64::from(height) * 4
                + u64::from(s.grid.0) * u64::from(s.grid.1) * 32
                + 8,
            Ordering::Relaxed,
        );
        Ok(s)
    }
    pub fn work_ready(&self, gl: &glow::Context) -> bool {
        !self.pending[self.cursor] || unsafe { gl.get_query_parameter_u32(self.queries[self.cursor], glow::QUERY_RESULT_AVAILABLE) != 0 }
    }
    pub fn texture(&self) -> u32 {
        self.textures[1].0.get()
    }
    pub fn render(
        &mut self,
        gl: &glow::Context,
        a: u32,
        b: u32,
        period: Duration,
        fraction: f32,
        estimate: bool,
        auto_fallback: bool,
        input_period: Duration,
    ) -> Result<()> {
        unsafe {
            // Reject a GPU that cannot sustain the experiment, not the underlying video.
            for i in 0..self.queries.len() {
                if self.pending[i]
                    && gl.get_query_parameter_u32(self.queries[i], glow::QUERY_RESULT_AVAILABLE)
                        != 0
                {
                    self.gpu_ns = gl.get_query_parameter_u64(self.queries[i], glow::QUERY_RESULT);
                    GPU_NS.store(self.gpu_ns, Ordering::Relaxed);
                    self.pending[i] = false;
                    let class = if self.estimate_query[i] { 0 } else { 1 };
                    // Estimate work occurs once per decoded pair, not once
                    // per generated frame; compare with its actual deadline.
                    let allowed = self.query_budget[i];
                    let over_budget = if self.estimate_query[i] {
                        policy::slow_estimate(self.gpu_ns, allowed)
                    } else {
                        policy::slow(self.gpu_ns, allowed)
                    };
                    self.slow[class] = if over_budget {
                        self.slow[class].saturating_add(1)
                    } else {
                        0
                    };
                }
            }
            anyhow::ensure!(
                !auto_fallback || self.slow.iter().all(|count| *count < 3),
                "GPU cannot sustain interpolation; native playback restored"
            );
            // Never accumulate unbounded work when the GPU stops returning queries.
            anyhow::ensure!(
                !self.pending[self.cursor],
                "Interpolation GPU queue is behind; native playback restored"
            );
            let state = State::capture(gl);
            for flag in [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::DEPTH_TEST,
                glow::STENCIL_TEST,
                glow::CULL_FACE,
            ] {
                gl.disable(flag);
            }
            gl.color_mask(true, true, true, true);
            gl.bind_vertex_array(self.vao);
            for (i, id) in [
                a,
                b,
                self.textures[0].0.get(),
                self.textures[2].0.get(),
                self.textures[4].0.get(),
                self.textures[5].0.get(),
                self.textures[6].0.get(),
            ]
            .into_iter()
            .enumerate()
            {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    std::num::NonZeroU32::new(id).map(glow::NativeTexture),
                );
            }
            let query = self.queries[self.cursor];
            gl.begin_query(glow::TIME_ELAPSED, query);
            // Build the two tiny, box-filtered images once per input pair.
            if estimate {
                let p = self.programs[4];
                gl.use_program(Some(p));
                gl.uniform_1_i32(gl.get_uniform_location(p, "previous_frame").as_ref(), 0);
                gl.uniform_1_i32(gl.get_uniform_location(p, "next_frame").as_ref(), 1);
                gl.uniform_2_f32(
                    gl.get_uniform_location(p, "image_size").as_ref(),
                    self.size.0 as f32,
                    self.size.1 as f32,
                );
                gl.viewport(0, 0, self.grid.0 as i32, self.grid.1 as i32);
                for side in 0..2 {
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbos[4 + side]));
                    gl.uniform_1_f32(gl.get_uniform_location(p, "fraction").as_ref(), side as f32);
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
            }
            // Raw estimate → scene consensus → edge medoid → seeds → output.
            for pass in [0, 4, 1, 2, 3] {
                if (pass != 3 && !estimate) || (pass == 3 && fraction <= 0.) {
                    continue;
                }
                // Raw estimate → edge-guided medoid → inverse-warp seeds → output.
                let target = [0, 3, 2, 1, 6][pass];
                let (w, h) = if pass == 3 {
                    self.size
                } else if pass == 4 {
                    (1, 1)
                } else {
                    self.grid
                };
                gl.active_texture(glow::TEXTURE2);
                gl.bind_texture(
                    glow::TEXTURE_2D,
                    Some(self.textures[if pass < 2 || pass == 4 { 0 } else { 3 }]),
                );
                let p = self.programs[if pass == 4 { 5 } else { pass }];
                gl.use_program(Some(p));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.fbos[target]));
                gl.viewport(0, 0, w as i32, h as i32);
                for (unit, name) in [
                    "previous_frame",
                    "next_frame",
                    "motion_field",
                    "seed_field",
                    "previous_preview",
                    "next_preview",
                    "camera_field",
                ]
                .into_iter()
                .enumerate()
                {
                    gl.uniform_1_i32(gl.get_uniform_location(p, name).as_ref(), unit as i32);
                }
                gl.uniform_1_f32(gl.get_uniform_location(p, "fraction").as_ref(), fraction);
                gl.uniform_2_f32(
                    gl.get_uniform_location(p, "image_size").as_ref(),
                    self.size.0 as f32,
                    self.size.1 as f32,
                );
                gl.draw_arrays(glow::TRIANGLES, 0, 3);
            }
            gl.end_query(glow::TIME_ELAPSED);
            self.pending[self.cursor] = true;
            self.estimate_query[self.cursor] = estimate;
            self.query_budget[self.cursor] = if estimate { input_period } else { period };
            self.cursor = (self.cursor + 1) % 3;
            state.restore(gl);
            if fraction > 0. && fraction < 1. {
                GENERATED.fetch_add(1, Ordering::Relaxed);
            }
            ACTIVE.store(true, Ordering::Relaxed);
            Ok(())
        }
    }
    pub fn release(&mut self, gl: &glow::Context) {
        unsafe {
            for f in self.fbos.drain(..) {
                gl.delete_framebuffer(f);
            }
            for t in self.textures.drain(..) {
                gl.delete_texture(t);
            }
            for p in self.programs.drain(..) {
                gl.delete_program(p);
            }
            for q in self.queries.drain(..) {
                gl.delete_query(q);
            }
            if let Some(v) = self.vao.take() {
                gl.delete_vertex_array(v);
            }
        }
        BYTES.store(0, Ordering::Relaxed);
        inactive();
    }
}
