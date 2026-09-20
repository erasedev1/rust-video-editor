// Effect passes: one full-screen draw each, reading the picture so far and
// writing the next one.
//
// Every pass shares this vertex stage and this uniform block. What differs is
// the fragment entry point, which is what the effect's registry kind dispatches
// to, and the meaning of the sixteen parameter floats, which the CPU packs per
// effect — see `effects.rs`, which is the only place that knows both sides.
//
// # Premultiplied throughout
//
// A pass writes premultiplied colour, because that is what the compositor's
// blend states expect and what makes a filtered edge correct: averaging
// straight colour across a transparent boundary drags the invisible pixels'
// colour into the visible ones, which is where the black fringe around a
// blurred logo comes from. The *input* may be either — the first effect in a
// chain reads a decoded frame, whose colour is independent of its coverage —
// so `tap` multiplies the alpha in when `premultiply` says to, and every later
// pass in the chain reads a texture that already carries it.

struct Effect {
    // Sixteen floats, packed per effect kind.
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    // One texel of the source, in UV.
    texel: vec2<f32>,
    // The source's size in pixels, so a pass can work in pixels where that is
    // what the parameter means — a feather is a distance on screen, not a
    // fraction of a frame that would stretch with the aspect ratio.
    size: vec2<f32>,
    // Whether the source's colour still has to have its alpha multiplied in.
    // A float rather than a bool because a uniform block is floats and vectors;
    // it is 0 or 1 and the branch on it is uniform across the draw.
    premultiply: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> fx: Effect;
@group(1) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(1) var source_sampler: sampler;

// Rec. 709 luma, which is what the video this edits is encoded against.
const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // The same triangle-strip corner order the compositor uses, but covering
    // the whole target: an effect pass has no geometry of its own.
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];

    var out: VertexOutput;
    out.clip_position = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

// One sample of the source, always premultiplied.
fn tap(uv: vec2<f32>) -> vec4<f32> {
    let s = textureSample(source, source_sampler, uv);
    if (fx.premultiply > 0.5) {
        return vec4<f32>(s.rgb * s.a, s.a);
    }
    return s;
}

// Straight colour, for the effects whose arithmetic is about colour rather
// than about coverage. Adjusting the exposure of a premultiplied pixel would
// brighten a half-transparent one half as much as an opaque one.
fn straight(c: vec4<f32>) -> vec4<f32> {
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(c.rgb / c.a, c.a);
}

fn premultiplied(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

// --- Gaussian blur -----------------------------------------------------------
//
// p0 = (sigma in source pixels, direction x, direction y, taps)
//
// Separable: the caller runs this twice, horizontally and then vertically,
// which is what turns an O(r²) kernel into two O(r) ones. `taps` is decided on
// the CPU from the radius and capped, so a 200-pixel blur costs a bounded
// number of samples: past the cap the kernel is sampled more sparsely rather
// than more expensively, which is the trade every real-time blur makes.
@fragment
fn fs_blur(in: VertexOutput) -> @location(0) vec4<f32> {
    let sigma = fx.p0.x;
    if (sigma <= 0.0) {
        return tap(in.uv);
    }
    let direction = fx.p0.yz * fx.texel;
    let taps = fx.p0.w;
    // Three standard deviations covers 99.7% of the kernel; past that the
    // weights are below what eight bits can represent.
    let support = sigma * 3.0;
    let step = max(support / taps, 0.5);

    var sum = vec4<f32>(0.0);
    var total = 0.0;
    var offset = -support;
    loop {
        if (offset > support) {
            break;
        }
        let weight = exp(-(offset * offset) / (2.0 * sigma * sigma));
        sum += tap(in.uv + direction * offset) * weight;
        total += weight;
        offset += step;
    }
    return sum / max(total, 1e-6);
}

// --- Colour adjust -----------------------------------------------------------
//
// p0 = (exposure gain, contrast factor, saturation, 1 / gamma)
// p1 = tint rgba
//
// Exposure and contrast arrive as multipliers rather than as the stops and the
// -1..1 the user dialled: the conversion is one line of CPU arithmetic and
// keeps a per-pixel `pow` and a branch out of the shader.
@fragment
fn fs_color(in: VertexOutput) -> @location(0) vec4<f32> {
    var c = straight(tap(in.uv));
    // Exposure, then contrast about mid grey, then gamma, then saturation,
    // then the tint. The order is the one a grade is built in: gain before
    // contrast so the pivot lands where the picture now sits, and saturation
    // after gamma so it acts on the tones being shown rather than the ones
    // being thrown away.
    var rgb = c.rgb * fx.p0.x;
    rgb = (rgb - 0.5) * fx.p0.y + 0.5;
    rgb = pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(fx.p0.w));
    let luma = dot(rgb, LUMA);
    rgb = mix(vec3<f32>(luma), rgb, fx.p0.z);
    rgb *= fx.p1.rgb;
    return premultiplied(vec4<f32>(rgb, c.a * fx.p1.a));
}

// --- Sharpen -----------------------------------------------------------------
//
// p0 = (amount, radius in source pixels, 0, 0)
//
// Unsharp mask against a 3x3 binomial kernel spread to `radius`: the difference
// between the picture and a slightly blurred copy of it *is* its detail, so
// adding a multiple of that difference back is sharpening.
@fragment
fn fs_sharpen(in: VertexOutput) -> @location(0) vec4<f32> {
    let centre = tap(in.uv);
    let amount = fx.p0.x;
    if (amount <= 0.0) {
        return centre;
    }
    let d = fx.texel * fx.p0.y;

    var blurred = centre * 4.0;
    blurred += tap(in.uv + vec2<f32>(-d.x, 0.0)) * 2.0;
    blurred += tap(in.uv + vec2<f32>(d.x, 0.0)) * 2.0;
    blurred += tap(in.uv + vec2<f32>(0.0, -d.y)) * 2.0;
    blurred += tap(in.uv + vec2<f32>(0.0, d.y)) * 2.0;
    blurred += tap(in.uv + vec2<f32>(-d.x, -d.y));
    blurred += tap(in.uv + vec2<f32>(d.x, -d.y));
    blurred += tap(in.uv + vec2<f32>(-d.x, d.y));
    blurred += tap(in.uv + vec2<f32>(d.x, d.y));
    blurred /= 16.0;

    let sharpened = centre + (centre - blurred) * amount;
    // Alpha is left alone: sharpening is about detail, and letting the kernel
    // ring on the coverage channel would eat holes in an otherwise opaque
    // picture.
    return vec4<f32>(clamp(sharpened.rgb, vec3<f32>(0.0), vec3<f32>(centre.a)), centre.a);
}

// --- Transform ---------------------------------------------------------------
//
// p0 = the inverse mapping's 2x2, row by row, in UV
// p1 = (offset x, offset y, opacity, 0)
//
// Inverse-mapped: for each pixel of the output, this asks where in the input it
// came from, which is the only way a fragment shader can resample. The whole
// matrix is built on the CPU — see `transform_pass` — so the shader is two dot
// products and a bounds test.
//
// What falls outside the source is transparent rather than clamped, because
// this moves the picture *within the clip's own frame*: sliding it left should
// reveal nothing, not a smear of its left-hand column.
@fragment
fn fs_transform(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = vec2<f32>(dot(fx.p0.xy, in.uv), dot(fx.p0.zw, in.uv)) + fx.p1.xy;
    if (src.x < 0.0 || src.x > 1.0 || src.y < 0.0 || src.y > 1.0) {
        return vec4<f32>(0.0);
    }
    return tap(src) * fx.p1.z;
}

// --- Shape mask --------------------------------------------------------------
//
// p0 = (centre x, centre y, half width, half height) in source pixels
// p1 = (cos, sin, feather in pixels, shape: 0 rectangle, 1 ellipse)
// p2 = (opacity, invert)
//
// Worked in pixels rather than in UV so that a feather is the same distance
// vertically as horizontally on a non-square frame, and so that the edge can be
// antialiased by a fixed one-pixel ramp when no feather is asked for.
@fragment
fn fs_mask(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.uv * fx.size - fx.p0.xy;
    // Into the shape's own frame. The mask turns; the picture does not.
    let r = vec2<f32>(
        p.x * fx.p1.x + p.y * fx.p1.y,
        -p.x * fx.p1.y + p.y * fx.p1.x,
    );
    let half = max(fx.p0.zw, vec2<f32>(1e-4));

    var distance: f32;
    if (fx.p1.w > 0.5) {
        // An ellipse has no closed-form signed distance. Scaling the normalised
        // radius by the smaller semi-axis is the standard approximation, and it
        // is exact on a circle — which is what a feathered vignette usually is.
        let k = length(r / half);
        distance = (k - 1.0) * min(half.x, half.y);
    } else {
        let q = abs(r) - half;
        distance = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
    }

    // At least one pixel of ramp, so an unfeathered edge is antialiased rather
    // than stepped. A mask is a shape at an arbitrary angle; a hard edge on one
    // would be the most visible aliasing in the frame.
    let feather = max(fx.p1.z, 1.0);
    var coverage = 1.0 - smoothstep(-feather * 0.5, feather * 0.5, distance);
    if (fx.p2.y > 0.5) {
        coverage = 1.0 - coverage;
    }
    // Opacity is how much of the hidden side is actually hidden, so a mask at
    // 0 changes nothing and one at 1 hides it completely.
    return tap(in.uv) * (1.0 - fx.p2.x * (1.0 - coverage));
}

// --- Luma key ----------------------------------------------------------------
//
// p0 = (threshold, threshold + softness, invert, 0)
//
// Brightness becomes coverage. The pair of edges is computed on the CPU so the
// shader cannot be handed a softness of zero, which would make the smoothstep
// undefined exactly where the key is hardest.
@fragment
fn fs_luma(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = straight(tap(in.uv));
    let luma = dot(c.rgb, LUMA);
    var keep = smoothstep(fx.p0.x, fx.p0.y, luma);
    if (fx.p0.z > 0.5) {
        keep = 1.0 - keep;
    }
    return premultiplied(vec4<f32>(c.rgb, c.a * keep));
}
