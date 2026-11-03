//! Draws YUV 4:2:0 frames with OpenGL: three single-channel textures and a
//! shader doing the colour conversion, so no RGB copy is ever made on the CPU.

use crate::player::Frame;
use eframe::glow::{self, HasContext};

const VERTEX: &str = r#"#version 140
out vec2 v_uv;
void main() {
    vec2 p = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    v_uv = vec2(p.x, 1.0 - p.y);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 140
in vec2 v_uv;
out vec4 color;
uniform sampler2D tex_y;
uniform sampler2D tex_u;
uniform sampler2D tex_v;
uniform mat3 yuv_to_rgb;
uniform vec3 offset;
void main() {
    vec3 yuv = vec3(texture(tex_y, v_uv).r, texture(tex_u, v_uv).r, texture(tex_v, v_uv).r) - offset;
    color = vec4(clamp(yuv_to_rgb * yuv, 0.0, 1.0), 1.0);
}
"#;

pub struct Renderer {
    program: glow::Program,
    vao: glow::VertexArray,
    textures: [glow::Texture; 3],
    sizes: [(i32, i32); 3],
    matrix: [f32; 9],
    offset: [f32; 3],
    pub has_frame: bool,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let program = gl.create_program()?;
            for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            let vao = gl.create_vertex_array()?;
            let textures = [gl.create_texture()?, gl.create_texture()?, gl.create_texture()?];
            for t in textures {
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            Ok(Renderer { program, vao, textures, sizes: [(0, 0); 3], matrix: [0.0; 9], offset: [0.0; 3], has_frame: false })
        }
    }

    pub fn upload(&mut self, gl: &glow::Context, f: &Frame) {
        let (w, h) = (f.width as i32, f.height as i32);
        let dims = [(w, h), ((w + 1) / 2, (h + 1) / 2), ((w + 1) / 2, (h + 1) / 2)];
        unsafe {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for (i, &(pw, ph)) in dims.iter().enumerate() {
                gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[i]));
                let data = glow::PixelUnpackData::Slice(Some(&f.planes[i]));
                if self.sizes[i] == (pw, ph) {
                    gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, pw, ph, glow::RED, glow::UNSIGNED_BYTE, data);
                } else {
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::R8 as i32, pw, ph, 0, glow::RED, glow::UNSIGNED_BYTE, data);
                    self.sizes[i] = (pw, ph);
                }
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        }
        // Y'CbCr -> R'G'B' (column-major), with the limited-range scaling folded in.
        let (kr, kb) = if f.bt709 { (0.2126, 0.0722) } else { (0.299, 0.114) };
        let kg = 1.0 - kr - kb;
        let (ys, cs) = if f.full_range { (1.0, 1.0) } else { (255.0 / 219.0, 255.0 / 224.0) };
        let (rv, bu) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
        let (gu, gv) = (-bu * kb / kg, -rv * kr / kg);
        self.matrix = [ys, ys, ys, 0.0, gu * cs, bu * cs, rv * cs, gv * cs, 0.0];
        let y0 = if f.full_range { 0.0 } else { 16.0 / 255.0 };
        self.offset = [y0, 128.0 / 255.0, 128.0 / 255.0];
        self.has_frame = true;
    }

    /// Draws the current frame over the whole viewport (egui sets it to the callback rect).
    pub fn paint(&self, gl: &glow::Context) {
        if !self.has_frame {
            return;
        }
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            for (i, name) in ["tex_y", "tex_u", "tex_v"].iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.textures[i]));
                gl.uniform_1_i32(gl.get_uniform_location(self.program, name).as_ref(), i as i32);
            }
            gl.uniform_matrix_3_f32_slice(gl.get_uniform_location(self.program, "yuv_to_rgb").as_ref(), false, &self.matrix);
            gl.uniform_3_f32_slice(gl.get_uniform_location(self.program, "offset").as_ref(), &self.offset);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            for i in (0..3).rev() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
        }
    }
}
