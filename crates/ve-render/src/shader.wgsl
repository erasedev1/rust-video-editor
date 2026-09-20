// Composites one textured, transformed, faded layer.
//
// The quad's four corners are generated from the vertex index rather than read
// from a vertex buffer: the geometry is always a unit quad, and the per-layer
// matrix is what gives it position, scale and rotation. That removes a vertex
// buffer binding from every draw.

struct Layer {
    // Maps the unit quad [0,1]^2 straight to clip space. Composed on the CPU
    // from the layer's anchor, scale, rotation, position and the composition
    // size, so the shader does no transform maths of its own.
    transform: mat4x4<f32>,
    // Multiplied into the sampled alpha. Separate from the matrix so that
    // animating a fade does not rebuild any geometry.
    opacity: f32,
    // Padded with scalars rather than a vec3. WGSL aligns a vec3<f32> to 16
    // bytes, which would push it past the opacity to offset 80 and make the
    // struct 96 bytes, silently disagreeing with the 80-byte Rust type that
    // fills it. Three scalars occupy exactly the 12 bytes that follow.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

// The uniform lives in its own group so that one buffer, addressed with a
// dynamic offset, serves every layer in a pass. The texture is in a separate
// group whose bind group is created once per texture and cached alongside it,
// so drawing a layer costs two set_bind_group calls and no allocation.
@group(0) @binding(0) var<uniform> layer: Layer;
@group(1) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(1) var source_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Triangle-strip order: top-left, top-right, bottom-left, bottom-right.
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[index];

    var out: VertexOutput;
    out.clip_position = layer.transform * vec4<f32>(corner, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(source, source_sampler, in.uv);
    let alpha = sampled.a * layer.opacity;
    // Premultiplied output, paired with a One / OneMinusSrcAlpha blend state.
    // The source here is a decoded frame, whose colour is independent of its
    // coverage, so its alpha is multiplied in on the way out. A source that
    // already carries its own alpha — the output of another pass — goes
    // through `fs_premultiplied` instead, which does not multiply it twice.
    return vec4<f32>(sampled.rgb * alpha, alpha);
}

// The same draw, for a source whose colour **already carries its alpha**: the
// output of another pass of this shader, which a nested composition and a
// motion-blurred layer both are.
//
// Multiplying by alpha again would apply it twice — a nested group at half
// opacity would come out at a quarter, and every motion blur would darken
// towards its own edges — so opacity scales the premultiplied value as a whole
// instead.
@fragment
fn fs_premultiplied(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, in.uv) * layer.opacity;
}
