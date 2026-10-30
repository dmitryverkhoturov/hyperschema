// Camera: centre split into hi + lo floats so (world - centre) stays exact
// far from the origin.
struct Cam {
    center_hi: vec2<f32>,
    center_lo: vec2<f32>,
    viewport: vec2<f32>,
    scale: f32,
    boost: f32,
};
@group(0) @binding(0) var<uniform> cam: Cam;

fn to_screen(w: vec2<f32>) -> vec2<f32> {
    return ((w - cam.center_hi) - cam.center_lo) * cam.scale + cam.viewport * 0.5;
}

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / cam.viewport.x * 2.0 - 1.0, 1.0 - p.y / cam.viewport.y * 2.0, 0.0, 1.0);
}

fn unpack(c: u32) -> vec4<f32> {
    return vec4<f32>(f32(c & 255u), f32((c >> 8u) & 255u), f32((c >> 16u) & 255u), f32(c >> 24u)) / 255.0;
}

fn corner(i: u32) -> vec2<f32> {
    var c = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0));
    return c[i];
}

// ---------------------------------------------------------------- shapes

struct RectIn {
    @location(0) xywh: vec4<f32>,
    @location(1) fill: u32,
    @location(2) stroke: u32,
    @location(3) shape: u32,   // kind | radius_world << 8
    @location(4) sw: f32,
};

struct RectOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) half: vec2<f32>,
    @location(2) @interpolate(flat) fill: vec4<f32>,
    @location(3) @interpolate(flat) stroke: vec4<f32>,
    // kind, stroke px, radius px, coverage (<1 for sub-pixel items)
    @location(4) @interpolate(flat) p: vec4<f32>,
};

@vertex
fn vs_rect(@builtin(vertex_index) vi: u32, r: RectIn) -> RectOut {
    let s = cam.scale;
    let real = r.xywh.zw * 0.5 * s;
    let half = max(real, vec2<f32>(0.5));
    let c = to_screen(r.xywh.xy + r.xywh.zw * 0.5);
    // Outlines stay visible down to a pixel: most schemes are stroke-only boxes.
    let sw = select(0.0, max(r.sw * s, 1.0), (r.stroke >> 24u) > 0u);
    let pad = 1.0 + sw;
    let local = corner(vi) * (half + pad);
    var o: RectOut;
    o.pos = to_clip(c + local);
    o.local = local;
    o.half = half;
    o.fill = unpack(r.fill);
    o.stroke = unpack(r.stroke);
    let cov = clamp(4.0 * real.x * real.y * cam.boost, 0.0, 1.0);
    o.p = vec4<f32>(f32(r.shape & 255u), sw, f32(r.shape >> 8u) * s, cov);
    return o;
}

fn sd_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_rect(i: RectOut) -> @location(0) vec4<f32> {
    let kind = u32(i.p.x);
    if (i.p.w < 1.0) {
        // Sub-pixel: a dot whose alpha is its real coverage.
        var c = i.fill;
        if (c.a == 0.0) { c = i.stroke; }
        let a = c.a * max(i.p.w, 0.08);
        return vec4<f32>(c.rgb * a, a);
    }
    var d: f32;
    if (kind == 1u) {
        let k = length(i.local / i.half);
        d = (k - 1.0) * min(i.half.x, i.half.y);
    } else if (kind == 2u) {
        let q = abs(i.local);
        d = (q.x / i.half.x + q.y / i.half.y - 1.0) * (i.half.x * i.half.y) / length(i.half);
    } else {
        d = sd_box(i.local, i.half, min(i.p.z, min(i.half.x, i.half.y)));
    }
    let sw = i.p.y;
    let fa = i.fill.a * clamp(0.5 - d, 0.0, 1.0);
    let sa = i.stroke.a * select(0.0, clamp(0.5 - (abs(d + sw * 0.5) - sw * 0.5), 0.0, 1.0), sw > 0.0);
    let rgb = i.stroke.rgb * sa + i.fill.rgb * fa * (1.0 - sa);
    let a = sa + fa * (1.0 - sa);
    return vec4<f32>(rgb, a);
}

// ---------------------------------------------------------------- lines

struct LineIn {
    @location(0) pts: vec4<f32>,
    @location(1) color: u32,
    @location(2) width: f32,
    @location(3) kind: u32,   // 0 segment, 1 arrowhead (pts = back point, tip)
};

struct LineOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,   // along, across (px)
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) p: vec4<f32>,   // len px, line px, kind, head px
};

@vertex
fn vs_line(@builtin(vertex_index) vi: u32, l: LineIn) -> LineOut {
    let a = to_screen(l.pts.xy);
    let b = to_screen(l.pts.zw);
    var dir = b - a;
    let len = length(dir);
    dir = select(vec2<f32>(1.0, 0.0), dir / len, len > 1e-4);
    let nrm = vec2<f32>(-dir.y, dir.x);
    let lw_real = l.width * cam.scale;
    let lw = max(lw_real, 1.0);
    var hw = lw * 0.5 + 1.0;
    if (l.kind == 1u) { hw = len * 0.6 + lw + 1.0; }
    let c = corner(vi);
    let along = select(-hw, len + hw, c.x > 0.0);
    let across = c.y * hw;
    let p = a + dir * along + nrm * across;
    var o: LineOut;
    o.pos = to_clip(p);
    // Too short to read: collapse the quad, nothing gets rasterised.
    if ((l.kind == 0u && len < 0.5) || (l.kind == 1u && len < 1.5)) {
        o.pos = vec4<f32>(2.0, 2.0, 0.0, 1.0);
    }
    o.uv = vec2<f32>(along, across);
    var col = unpack(l.color);
    col.a = col.a * clamp(lw_real * 2.0, 0.55, 1.0);
    o.color = col;
    o.p = vec4<f32>(len, lw, f32(l.kind), len);
    return o;
}

fn seg_dist(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * t);
}

@fragment
fn fs_line(i: LineOut) -> @location(0) vec4<f32> {
    let len = i.p.x;
    let lw = i.p.y;
    var d: f32;
    if (i.p.z < 0.5) {
        d = seg_dist(i.uv, vec2<f32>(0.0, 0.0), vec2<f32>(len, 0.0)) - lw * 0.5;
    } else {
        let tip = vec2<f32>(len, 0.0);
        let d1 = seg_dist(i.uv, tip, vec2<f32>(0.0, len * 0.45));
        let d2 = seg_dist(i.uv, tip, vec2<f32>(0.0, -len * 0.45));
        d = min(d1, d2) - lw * 0.5;
    }
    let a = i.color.a * clamp(0.5 - d, 0.0, 1.0);
    return vec4<f32>(i.color.rgb * a, a);
}

// ---------------------------------------------------------------- glyphs

@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct GlyphIn {
    @location(0) xywh: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: u32,
    @location(3) sdf: u32,
};

struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
    @location(2) @interpolate(flat) sdf: u32,
};

@vertex
fn vs_glyph(@builtin(vertex_index) vi: u32, g: GlyphIn) -> GlyphOut {
    let c = corner(vi) * 0.5 + vec2<f32>(0.5);
    let w = g.xywh.xy + g.xywh.zw * c;
    var o: GlyphOut;
    o.pos = to_clip(to_screen(w));
    o.uv = mix(g.uv.xy, g.uv.zw, c);
    o.color = unpack(g.color);
    o.sdf = g.sdf;
    return o;
}

// Distance field reach in raster pixels (text.rs SDF_SPREAD).
const SDF_SPREAD: f32 = 16.0;

@fragment
fn fs_glyph(i: GlyphOut) -> @location(0) vec4<f32> {
    let v = textureSample(atlas, samp, i.uv).r;
    // Distance field: distance in raster pixels, edge width from how fast it
    // changes on screen. Derivatives are taken outside the branch.
    let d = (v - 0.5) * 2.0 * SDF_SPREAD;
    // Capped: only magnified glyphs use the field, and the helper pixels
    // past a quad's edge must not smear its border into a faint frame.
    let fw = clamp(length(vec2<f32>(dpdx(d), dpdy(d))), 1e-4, 1.5);
    var cov = v;
    if (i.sdf == 1u) {
        cov = clamp(d / fw + 0.5, 0.0, 1.0);
    }
    let a = cov * i.color.a;
    return vec4<f32>(i.color.rgb * a, a);
}
