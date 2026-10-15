//! Aurora background, drawn with OpenGL behind the whole window.
//!
//! The shader is ported from the "Aurora" background of React Bits
//! (https://reactbits.dev, MIT + Commons Clause). Pointer interaction is left
//! out; height, brightness and speed are driven by the music instead, and a
//! vertical mask keeps the light away from the text and controls.

use std::sync::Arc;

use eframe::egui::{self, Rect};
use eframe::egui_glow;
use eframe::glow::{self, HasContext};

pub const STOPS: usize = 4;

/// Everything the shader needs for one frame.
#[derive(Clone, Copy)]
pub struct Params {
    pub time: f32,
    /// sRGB color stops, left to right.
    pub stops: [[f32; 3]; STOPS],
    pub amplitude: f32,
    /// Edge softness (0 sharp .. 1 soft).
    pub blend: f32,
    pub height: f32,
    pub glow: f32,
    pub brightness: f32,
    pub opacity: f32,
    /// Intro progress (0..1).
    pub reveal: f32,
    /// Fractions of the window height (from the top) where the readable
    /// zones end / start: the light is dimmed above `clear_top` and below
    /// `clear_bottom`.
    pub clear_top: f32,
    pub clear_bottom: f32,
}

pub struct Aurora {
    program: glow::Program,
    vao: glow::VertexArray,
    loc: Locations,
}

struct Locations {
    resolution: Option<glow::UniformLocation>,
    origin: Option<glow::UniformLocation>,
    time: Option<glow::UniformLocation>,
    stops: Option<glow::UniformLocation>,
    stop_count: Option<glow::UniformLocation>,
    amplitude: Option<glow::UniformLocation>,
    blend: Option<glow::UniformLocation>,
    height: Option<glow::UniformLocation>,
    scale: Option<glow::UniformLocation>,
    glow: Option<glow::UniformLocation>,
    brightness: Option<glow::UniformLocation>,
    layers: Option<glow::UniformLocation>,
    reveal: Option<glow::UniformLocation>,
    opacity: Option<glow::UniformLocation>,
    clear: Option<glow::UniformLocation>,
}

const VERTEX: &str = r#"
void main() {
    // One triangle covering the viewport, no vertex buffer needed.
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
uniform vec2 uResolution;
uniform vec2 uOrigin;
uniform float uTime;
uniform vec3 uStops[4];
uniform int uStopCount;
uniform float uAmplitude;
uniform float uBlend;
uniform float uHeight;
uniform float uScale;
uniform float uGlow;
uniform float uBrightness;
uniform int uLayers;
uniform float uReveal;
uniform float uOpacity;
uniform vec2 uClear;
out vec4 outColor;

vec3 permute(vec3 x) {
  return mod(((x * 34.0) + 1.0) * x, 289.0);
}

float snoise(vec2 v) {
  const vec4 C = vec4(0.211324865405187, 0.366025403784439, -0.577350269189626, 0.024390243902439);
  vec2 i = floor(v + dot(v, C.yy));
  vec2 x0 = v - i + dot(i, C.xx);
  vec2 i1 = (x0.x > x0.y) ? vec2(1.0, 0.0) : vec2(0.0, 1.0);
  vec4 x12 = x0.xyxy + C.xxzz;
  x12.xy -= i1;
  i = mod(i, 289.0);
  vec3 p = permute(permute(i.y + vec3(0.0, i1.y, 1.0)) + i.x + vec3(0.0, i1.x, 1.0));
  vec3 m = max(0.5 - vec3(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)), 0.0);
  m = m * m;
  m = m * m;
  vec3 x = 2.0 * fract(p * C.www) - 1.0;
  vec3 h = abs(x) - 0.5;
  vec3 ox = floor(x + 0.5);
  vec3 a0 = x - ox;
  m *= 1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h);
  vec3 g;
  g.x = a0.x * x0.x + h.x * x0.y;
  g.yz = a0.yz * x12.xz + h.yz * x12.yw;
  return 130.0 * dot(m, g);
}

float smootherstep(float edge0, float edge1, float x) {
  float t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
  return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

vec3 oklabToLinear(vec3 c) {
  float l = c.x + 0.3963377774 * c.y + 0.2158037573 * c.z;
  float m = c.x - 0.1055613458 * c.y - 0.0638541728 * c.z;
  float s = c.x - 0.0894841775 * c.y - 1.2914855480 * c.z;
  l = l * l * l;
  m = m * m * m;
  s = s * s * s;
  return vec3(
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s
  );
}

vec3 toSrgb(vec3 c) {
  c = clamp(c, 0.0, 1.0);
  return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
}

vec3 ramp(float t) {
  float x = clamp(t, 0.0, 1.0) * float(uStopCount - 1);
  int i = int(min(floor(x), float(uStopCount - 2)));
  float f = x - float(i);
  f = f * f * (3.0 - 2.0 * f);
  vec3 lch = mix(uStops[i], uStops[i + 1], f);
  return toSrgb(oklabToLinear(vec3(lch.x, lch.y * cos(lch.z), lch.y * sin(lch.z))));
}

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

void main() {
  vec2 frag = gl_FragCoord.xy - uOrigin;
  float y = 1.0 - frag.y / uResolution.y;
  float u = frag.x / uResolution.x;
  float X = (frag.x - uResolution.x * 0.5) / uResolution.y;
  float t = uTime;

  vec3 glow = vec3(0.0);
  float cover = 0.0;
  for (int k = 0; k < 3; k++) {
    if (k >= uLayers) break;
    float layer = float(k);
    float seed = layer * 13.7;
    float pace = t * (1.0 - layer * 0.18);
    float warp = snoise(vec2(X * 0.45 * uScale + pace * 0.02 + seed, pace * 0.05 + seed));
    float wave = snoise(vec2(X * 0.75 * uScale + warp * 0.35 - pace * 0.03 + seed, pace * 0.07 - seed)) * 0.72
      + snoise(vec2(X * 1.6 * uScale + warp * 0.5 + pace * 0.04 - seed, pace * 0.1 + seed * 0.5)) * 0.28;
    float hem = uHeight + layer * 0.07 + wave * 0.15 * uAmplitude;
    hem = mix(-0.25, hem, uReveal);

    float soft = (0.04 + uBlend * 0.16) * (1.0 + layer * 0.9);
    float rise = hem - y;
    float low = -soft - uGlow * 0.14;
    float high = 0.26 + soft * 0.6;
    float body = pow(smootherstep(low, high, rise), 1.5 + uGlow * 0.6);
    float lobes = 0.86 + 0.14 * snoise(vec2(X * 0.9 * uScale - pace * 0.025 + seed, y * 0.3 + pace * 0.04));
    float alpha = clamp(body * lobes * (layer < 0.5 ? 1.0 : 0.32 / layer), 0.0, 1.0);

    vec3 tint = ramp(u + 0.05 * sin(pace * 0.08 + layer * 2.3) + layer * 0.1 + max(rise, 0.0) * 0.05);
    glow = 1.0 - (1.0 - glow) * (1.0 - tint * alpha);
    cover = 1.0 - (1.0 - cover) * (1.0 - alpha);
  }

  // Keep the light away from the header and the waveform / controls.
  float clearing = smoothstep(uClear.x - 0.04, uClear.x + 0.10, y)
    * (1.0 - smoothstep(uClear.y - 0.10, uClear.y + 0.02, y));
  float mask = 0.08 + 0.92 * clearing;

  float fade = smootherstep(0.0, 0.7, uReveal) * uOpacity * mask;
  float strength = clamp(uBrightness, 0.0, 2.0);
  glow *= fade * min(strength, 1.0);
  cover *= fade * min(strength, 1.0);
  glow = 1.0 - pow(1.0 - glow, vec3(max(strength, 1.0)));
  float dither = (hash(gl_FragCoord.xy) - 0.5) / 255.0;
  outColor = vec4(glow + dither, clamp(cover + dither, 0.0, 1.0));
}
"#;

impl Aurora {
    pub fn new(gl: &glow::Context) -> Result<Arc<Aurora>, String> {
        let header = if gl.version().is_embedded {
            "#version 300 es\nprecision highp float;\nprecision highp int;\n"
        } else {
            "#version 330 core\n"
        };
        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, src) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, FRAGMENT),
            ] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, &format!("{header}{src}"));
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return Err(gl.get_shader_info_log(shader));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            let vao = gl.create_vertex_array()?;
            let u = |name: &str| gl.get_uniform_location(program, name);
            let loc = Locations {
                resolution: u("uResolution"),
                origin: u("uOrigin"),
                time: u("uTime"),
                stops: u("uStops"),
                stop_count: u("uStopCount"),
                amplitude: u("uAmplitude"),
                blend: u("uBlend"),
                height: u("uHeight"),
                scale: u("uScale"),
                glow: u("uGlow"),
                brightness: u("uBrightness"),
                layers: u("uLayers"),
                reveal: u("uReveal"),
                opacity: u("uOpacity"),
                clear: u("uClear"),
            };
            Ok(Arc::new(Aurora { program, vao, loc }))
        }
    }

    /// Queues the aurora for `rect` (normally the whole window).
    pub fn paint(self: &Arc<Self>, painter: &egui::Painter, rect: Rect, params: Params) {
        let aurora = self.clone();
        let stops = oklch_stops(&params.stops);
        let callback = egui_glow::CallbackFn::new(move |info, p| {
            let vp = info.viewport_in_pixels();
            unsafe { aurora.draw(p.gl(), &params, &stops, &vp) };
        });
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }

    unsafe fn draw(
        &self,
        gl: &glow::Context,
        p: &Params,
        stops: &[f32; STOPS * 3],
        vp: &egui::epaint::ViewportInPixels,
    ) {
        let l = &self.loc;
        unsafe {
            gl.use_program(Some(self.program));
            gl.uniform_2_f32(
                l.resolution.as_ref(),
                vp.width_px as f32,
                vp.height_px as f32,
            );
            gl.uniform_2_f32(
                l.origin.as_ref(),
                vp.left_px as f32,
                vp.from_bottom_px as f32,
            );
            gl.uniform_1_f32(l.time.as_ref(), p.time);
            gl.uniform_3_f32_slice(l.stops.as_ref(), stops);
            gl.uniform_1_i32(l.stop_count.as_ref(), STOPS as i32);
            gl.uniform_1_f32(l.amplitude.as_ref(), p.amplitude);
            gl.uniform_1_f32(l.blend.as_ref(), p.blend);
            gl.uniform_1_f32(l.height.as_ref(), p.height);
            gl.uniform_1_f32(l.scale.as_ref(), 1.0);
            gl.uniform_1_f32(l.glow.as_ref(), p.glow);
            gl.uniform_1_f32(l.brightness.as_ref(), p.brightness);
            gl.uniform_1_i32(l.layers.as_ref(), 2);
            gl.uniform_1_f32(l.reveal.as_ref(), 1.0 - (1.0 - p.reveal).powi(3));
            gl.uniform_1_f32(l.opacity.as_ref(), p.opacity);
            gl.uniform_2_f32(l.clear.as_ref(), p.clear_top, p.clear_bottom);
            // egui leaves premultiplied-alpha blending on, which is what the
            // shader outputs.
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_vertex_array(None);
        }
    }
}

/// sRGB stops to OKLCH, with hues unwrapped so blends take the short way.
fn oklch_stops(stops: &[[f32; 3]; STOPS]) -> [f32; STOPS * 3] {
    let lch: Vec<[f32; 3]> = stops.iter().map(|c| to_oklch(*c)).collect();
    let known = lch.iter().position(|s| s[1] > 0.02);
    let mut previous = known.map_or(0.0, |i| lch[i][2]);
    let mut out = [0.0; STOPS * 3];
    for (i, s) in lch.iter().enumerate() {
        let mut hue = if s[1] > 0.02 { s[2] } else { previous };
        while hue - previous > std::f32::consts::PI {
            hue -= std::f32::consts::TAU;
        }
        while hue - previous < -std::f32::consts::PI {
            hue += std::f32::consts::TAU;
        }
        previous = hue;
        out[i * 3..i * 3 + 3].copy_from_slice(&[s[0], s[1], hue]);
    }
    out
}

fn to_oklch(rgb: [f32; 3]) -> [f32; 3] {
    let lin = rgb.map(|c| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let [r, g, b] = lin;
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let lab = [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ];
    [lab[0], lab[1].hypot(lab[2]), lab[2].atan2(lab[1])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklch_of_white_and_gold() {
        let w = to_oklch([1.0, 1.0, 1.0]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1] < 1e-3);
        let g = to_oklch([0.925, 0.73, 0.3]);
        assert!(g[0] > 0.7 && g[0] < 0.9 && g[1] > 0.08, "{g:?}");
    }
}
