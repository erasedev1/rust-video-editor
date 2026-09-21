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

// --- Three-way colour --------------------------------------------------------
//
// p0 = (slope rgb, 0) — what a unit of input is worth, i.e. gain − lift
// p1 = (lift rgb, 0)  — what black becomes
// p2 = (exponent rgb, 0)
//
// Lift, gamma and gain, the grade every colourist builds first:
//
//     out = (in · (gain − lift) + lift) ^ (1/gamma)
//
// in = 0 comes out at `lift` and in = 1 at `gain`, so the two ends of the range
// are dialled directly and the exponent bends what lies between them. Each is
// per channel, which is what makes a *colour* wheel rather than three sliders:
// a warm shadow is a lift whose red is higher than its blue.
//
// The three vectors are built on the CPU from the six controls — see
// `three_way_pass`. Doing it there rather than here keeps a `pow` of a constant
// out of every pixel, and puts the arithmetic where a test can read it.
@fragment
fn fs_grade(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = straight(tap(in.uv));
    // Negative before the exponent is not "very dark", it is NaN. A lift that
    // has pushed a channel below zero is clamped here rather than being left to
    // come out of `pow` as a hole in the picture.
    let graded = pow(max(c.rgb * fx.p0.xyz + fx.p1.xyz, vec3<f32>(0.0)), fx.p2.xyz);
    return premultiplied(vec4<f32>(graded, c.a));
}

// --- HSL secondary -----------------------------------------------------------
//
// p0 = (hue centre in turns, half width in turns, softness in turns, saturation floor)
// p1 = (hue shift in turns, saturation scale, luma scale, show matte)
//
// A secondary is two halves: deciding *what* is selected, and grading it. The
// first half is the one that is hard to see, which is why `show matte` exists —
// dialling a qualifier by looking at the graded picture means guessing at the
// edges of the selection from the other side of a grade.
//
// Hue is carried in turns rather than degrees so the wrap is `fract`: red sits
// at both 0 and 1, and a band centred on it has to reach across that seam
// rather than stopping dead at it.
fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let high = max(c.r, max(c.g, c.b));
    let low = min(c.r, min(c.g, c.b));
    let chroma = high - low;
    let lightness = (high + low) * 0.5;

    var hue = 0.0;
    if (chroma > 1e-6) {
        if (high == c.r) {
            hue = (c.g - c.b) / chroma;
            // The one branch that wraps: red's sector runs from −1 to +1 and
            // the negative half belongs at the top of the circle.
            hue = hue - 6.0 * floor(hue / 6.0);
        } else if (high == c.g) {
            hue = (c.b - c.r) / chroma + 2.0;
        } else {
            hue = (c.r - c.g) / chroma + 4.0;
        }
        hue = hue / 6.0;
    }

    var saturation = 0.0;
    let span = 1.0 - abs(2.0 * lightness - 1.0);
    if (span > 1e-6) {
        saturation = chroma / span;
    }
    return vec3<f32>(hue, saturation, lightness);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let hue = fract(hsl.x);
    let saturation = clamp(hsl.y, 0.0, 1.0);
    let lightness = clamp(hsl.z, 0.0, 1.0);
    let chroma = (1.0 - abs(2.0 * lightness - 1.0)) * saturation;
    let sector = hue * 6.0;
    let second = chroma * (1.0 - abs(fract(sector * 0.5) * 2.0 - 1.0));
    let base = lightness - chroma * 0.5;

    var rgb = vec3<f32>(0.0);
    if (sector < 1.0) {
        rgb = vec3<f32>(chroma, second, 0.0);
    } else if (sector < 2.0) {
        rgb = vec3<f32>(second, chroma, 0.0);
    } else if (sector < 3.0) {
        rgb = vec3<f32>(0.0, chroma, second);
    } else if (sector < 4.0) {
        rgb = vec3<f32>(0.0, second, chroma);
    } else if (sector < 5.0) {
        rgb = vec3<f32>(second, 0.0, chroma);
    } else {
        rgb = vec3<f32>(chroma, 0.0, second);
    }
    return rgb + base;
}

@fragment
fn fs_qualify(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = straight(tap(in.uv));
    let hsl = rgb_to_hsl(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));

    // Distance round the circle, never more than half a turn: a band centred on
    // red must reach 0.98 as readily as 0.02.
    let raw = abs(hsl.x - fx.p0.x);
    let distance = min(raw, 1.0 - raw);

    let half_width = fx.p0.y;
    let softness = fx.p0.z;
    // Inside the band the weight is 1; past it the weight ramps to 0 over the
    // softness. A softness of zero gives a hard edge, which is what `smoothstep`
    // with equal edges would make undefined — so the pair is widened on the CPU.
    var weight = 1.0 - smoothstep(half_width, half_width + softness, distance);

    // Saturation gates the selection. Grey has no meaningful hue, so without
    // this a band centred anywhere would select every neutral in the frame.
    weight *= smoothstep(fx.p0.w, fx.p0.w + 0.05, hsl.y);

    if (fx.p1.w > 0.5) {
        // The matte, in the picture's own alpha: what is selected is white and
        // what is not is black, so the edges of the key are the thing on screen
        // rather than something to be inferred from a grade.
        return premultiplied(vec4<f32>(vec3<f32>(weight), c.a));
    }

    let shifted = hsl_to_rgb(vec3<f32>(
        hsl.x + fx.p1.x,
        hsl.y * fx.p1.y,
        hsl.z * fx.p1.z,
    ));
    // Mixed by the weight rather than written through, so the edge of the
    // selection is a ramp and not a cut-out.
    return premultiplied(vec4<f32>(mix(c.rgb, shifted, weight), c.a));
}
