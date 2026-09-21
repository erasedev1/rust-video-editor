//! Effect passes, rendered on a real device and read back.
//!
//! The unit tests in `effects.rs` check what is *packed* into a pass; these
//! check what it draws. Both halves are needed, because the two can disagree
//! silently: a parameter in the wrong slot produces a picture, just not the one
//! that was asked for.
//!
//! Pixels are read back **premultiplied**, because that is what every pass
//! writes — see the note at the top of `effects.wgsl`. So a half-covered red
//! pixel reads as `[128, 0, 0, 128]`, not `[255, 0, 0, 128]`.

use std::sync::Arc;

use ve_core::registry::kinds;
use ve_core::{builtin_registry, ColorSpace, EffectId, EffectState, ParamValue, Size, Vec2};
use ve_media::{PixelFormat, VideoFrame};
use ve_render::{chain_passes, CompositeKey, GpuContext, GpuTexture, RenderTarget, Renderer};
use ve_time::Ticks;

fn gpu() -> &'static GpuContext {
    use std::sync::OnceLock;
    static GPU: OnceLock<Option<GpuContext>> = OnceLock::new();
    GPU.get_or_init(|| match GpuContext::headless() {
        Ok(ctx) => Some(ctx),
        Err(e) => panic!("no GPU adapter available for rendering tests: {e}"),
    })
    .as_ref()
    .unwrap()
}

/// A frame whose pixel at `(x, y)` is whatever `paint` says.
fn frame(size: Size, paint: impl Fn(u32, u32) -> [u8; 4]) -> VideoFrame {
    let stride = size.width * 4;
    let mut data = Vec::with_capacity((stride * size.height) as usize);
    for y in 0..size.height {
        for x in 0..size.width {
            data.extend_from_slice(&paint(x, y));
        }
    }
    VideoFrame::new(
        Arc::from(data.into_boxed_slice()),
        size,
        stride,
        PixelFormat::Rgba8,
        Ticks::ZERO,
    )
}

fn solid(size: Size, colour: [u8; 4]) -> VideoFrame {
    frame(size, |_, _| colour)
}

/// An effect of `kind` at its declared defaults, with `overrides` applied.
fn effect(kind: &str, overrides: &[(&str, ParamValue)]) -> EffectState {
    let registry = builtin_registry();
    let mut effect = registry.instantiate(kind, EffectId::from_raw(1)).expect("a built-in");
    for (key, value) in overrides {
        *effect.param_mut(key).unwrap_or_else(|| panic!("no parameter {key}")) = value.clone();
    }
    effect.evaluate(Ticks::ZERO, registry.get(kind))
}

/// Runs a chain over a frame and reads the result back.
///
/// Each pass draws into a target of its own and the next one samples it, which
/// is the ping-pong the preview does — including the part that matters most,
/// that a pass after the first reads a texture whose colour already carries its
/// alpha.
fn run(size: Size, source: &VideoFrame, effects: &[EffectState]) -> Vec<u8> {
    run_in(ColorSpace::Perceptual, size, source, effects)
}

fn run_in(
    space: ColorSpace,
    size: Size,
    source: &VideoFrame,
    effects: &[EffectState],
) -> Vec<u8> {
    let gpu = gpu();
    let mut renderer = Renderer::new(&gpu.device);
    let passes = chain_passes(effects, size);
    assert!(!passes.is_empty(), "the chain under test draws nothing");

    let mut texture: GpuTexture = renderer.upload(&gpu.device, &gpu.queue, source);
    let mut targets: Vec<RenderTarget> = Vec::new();
    for pass in &passes {
        let target = RenderTarget::new(&gpu.device, size);
        renderer.apply_effect(&gpu.device, &gpu.queue, &target, space, &texture, pass);
        // Identified by something that changes per pass, as the render cache
        // would identify it by the pass's own key.
        texture = renderer.bind_target(
            &gpu.device,
            &target,
            CompositeKey::of_effect(texture.id(), size, space, pass),
        );
        targets.push(target);
    }
    targets.last().expect("one pass at least").read_pixels(&gpu.device, &gpu.queue)
}

fn pixel(pixels: &[u8], size: Size, x: u32, y: u32) -> [u8; 4] {
    RenderTarget::pixel_at(pixels, size, x, y)
}

#[track_caller]
fn assert_close(got: [u8; 4], want: [u8; 4], what: &str) {
    let close = got.iter().zip(want.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 3);
    assert!(close, "{what}: got {got:?}, expected {want:?}");
}

const SIZE: Size = Size::new(64, 64);
const RED: [u8; 4] = [255, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];

// --- Blur --------------------------------------------------------------------

#[test]
fn a_blur_spreads_an_edge_and_leaves_what_is_far_from_it_alone() {
    // Left half white, right half black, blurred horizontally.
    let source = frame(SIZE, |x, _| if x < 32 { WHITE } else { BLACK });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::GAUSSIAN_BLUR,
            &[("radius", ParamValue::scalar(4.0)), ("direction", ParamValue::Choice(1))],
        )],
    );

    // The edge itself lands halfway between the two.
    let at_edge = pixel(&pixels, SIZE, 31, 32);
    assert!((100..=155).contains(&at_edge[0]), "the edge should be mid grey, got {at_edge:?}");
    // Well away from it, nothing changed.
    assert_close(pixel(&pixels, SIZE, 2, 32), WHITE, "far left");
    assert_close(pixel(&pixels, SIZE, 61, 32), BLACK, "far right");
    // And a blur is not a fade: the picture stays opaque.
    assert_eq!(at_edge[3], 255, "blurring must not eat the alpha");
}

#[test]
fn a_horizontal_blur_leaves_a_horizontal_edge_alone() {
    // The same picture turned on its side, blurred along the axis it does not
    // vary in: the only thing that can change is nothing.
    let source = frame(SIZE, |_, y| if y < 32 { WHITE } else { BLACK });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::GAUSSIAN_BLUR,
            &[("radius", ParamValue::scalar(6.0)), ("direction", ParamValue::Choice(1))],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 32, 30), WHITE, "just above the edge");
    assert_close(pixel(&pixels, SIZE, 32, 33), BLACK, "just below it");
}

#[test]
fn blurring_across_transparency_does_not_darken_the_colour() {
    // The premultiplication test. Blurring straight colour across the boundary
    // between opaque red and clear pixels drags their colour — black — into the
    // red, which is where a blurred logo gets its dark fringe. Blurred
    // premultiplied, the colour stays red and only the coverage falls off, so
    // every pixel of the smear has red equal to its own alpha.
    let source = frame(SIZE, |x, _| if x < 32 { RED } else { TRANSPARENT });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::GAUSSIAN_BLUR,
            &[("radius", ParamValue::scalar(5.0)), ("direction", ParamValue::Choice(1))],
        )],
    );

    for x in 28..36 {
        let p = pixel(&pixels, SIZE, x, 32);
        assert!(
            (p[0] as i32 - p[3] as i32).abs() <= 3,
            "at x={x} the red should equal the coverage, got {p:?}"
        );
        assert_eq!([p[1], p[2]], [0, 0], "nothing but red went in");
    }
}

// --- Colour adjust -----------------------------------------------------------

#[test]
fn exposure_is_a_doubling_per_stop() {
    let source = solid(SIZE, [64, 64, 64, 255]);
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::COLOR_ADJUST, &[("exposure", ParamValue::scalar(1.0))])],
    );
    assert_close(pixel(&pixels, SIZE, 32, 32), [128, 128, 128, 255], "one stop up");
}

#[test]
fn saturation_at_zero_leaves_the_luma_it_started_with() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::COLOR_ADJUST, &[("saturation", ParamValue::scalar(0.0))])],
    );
    // Rec. 709: red carries 0.2126 of the luma.
    let grey = (0.2126_f64 * 255.0).round() as u8;
    assert_close(pixel(&pixels, SIZE, 32, 32), [grey, grey, grey, 255], "monochrome");
}

#[test]
fn a_tint_multiplies_and_white_changes_nothing() {
    let source = solid(SIZE, WHITE);
    let untouched = run(SIZE, &source, &[effect(kinds::COLOR_ADJUST, &[])]);
    assert_close(pixel(&untouched, SIZE, 32, 32), WHITE, "the defaults are the identity");

    let tinted = run(
        SIZE,
        &source,
        &[effect(
            kinds::COLOR_ADJUST,
            &[("tint", ParamValue::color(ve_core::Rgba::new(1.0, 0.5, 0.0, 1.0)))],
        )],
    );
    assert_close(pixel(&tinted, SIZE, 32, 32), [255, 128, 0, 255], "tinted");
}

#[test]
fn a_colour_adjustment_does_not_depend_on_coverage() {
    // Half-transparent grey and opaque grey are the same colour; brightening
    // them has to brighten them by the same amount, which is only true if the
    // pass unpremultiplies first.
    let source = frame(SIZE, |x, _| if x < 32 { [64, 64, 64, 255] } else { [64, 64, 64, 128] });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::COLOR_ADJUST, &[("exposure", ParamValue::scalar(1.0))])],
    );

    let opaque = pixel(&pixels, SIZE, 16, 32);
    let half = pixel(&pixels, SIZE, 48, 32);
    assert_close(opaque, [128, 128, 128, 255], "opaque");
    // Premultiplied on the way out, so half coverage reads half the value.
    assert_close(half, [64, 64, 64, 128], "half covered");
}

// --- Sharpen -----------------------------------------------------------------

#[test]
fn sharpening_overshoots_on_both_sides_of_an_edge() {
    let source =
        frame(SIZE, |x, _| if x < 32 { [100, 100, 100, 255] } else { [155, 155, 155, 255] });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHARPEN,
            &[("amount", ParamValue::scalar(1.0)), ("radius", ParamValue::scalar(1.0))],
        )],
    );

    let dark_side = pixel(&pixels, SIZE, 31, 32);
    let light_side = pixel(&pixels, SIZE, 32, 32);
    assert!(dark_side[0] < 100, "the dark side should be pushed darker: {dark_side:?}");
    assert!(light_side[0] > 155, "the light side lighter: {light_side:?}");
    // Away from any detail there is nothing to sharpen.
    assert_close(pixel(&pixels, SIZE, 8, 32), [100, 100, 100, 255], "flat area");
}

// --- Transform ---------------------------------------------------------------

#[test]
fn a_transform_moves_the_picture_within_the_frame_and_crops_it() {
    // Left half red, right half white, slid a quarter of the frame right.
    let source = frame(SIZE, |x, _| if x < 32 { RED } else { WHITE });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::TRANSFORM, &[("position", ParamValue::point(Vec2::new(16.0, 0.0)))])],
    );

    // What was at x=24 is now at x=40.
    assert_close(pixel(&pixels, SIZE, 40, 32), RED, "moved red");
    assert_close(pixel(&pixels, SIZE, 56, 32), WHITE, "moved white");
    // What slid in from the left is nothing at all, not a smear of the edge.
    assert_close(pixel(&pixels, SIZE, 4, 32), TRANSPARENT, "vacated");
}

#[test]
fn a_transform_scales_about_its_anchor() {
    // A red square in the middle third of a white frame, doubled about the
    // centre: the square now covers two thirds.
    let source =
        frame(
            SIZE,
            |x, y| {
                if (21..43).contains(&x) && (21..43).contains(&y) {
                    RED
                } else {
                    WHITE
                }
            },
        );
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::TRANSFORM, &[("scale", ParamValue::point(Vec2::splat(2.0)))])],
    );

    assert_close(pixel(&pixels, SIZE, 32, 32), RED, "centre");
    assert_close(pixel(&pixels, SIZE, 14, 32), RED, "grown left");
    assert_close(pixel(&pixels, SIZE, 49, 32), RED, "grown right");
    assert_close(pixel(&pixels, SIZE, 4, 32), WHITE, "still outside");
}

// --- Masks and mattes --------------------------------------------------------

#[test]
fn a_rectangular_mask_keeps_what_is_inside_it() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[("shape", ParamValue::Choice(0)), ("size", ParamValue::point(Vec2::splat(0.5)))],
        )],
    );

    assert_close(pixel(&pixels, SIZE, 32, 32), RED, "inside");
    assert_close(pixel(&pixels, SIZE, 2, 2), TRANSPARENT, "outside");
    assert_close(pixel(&pixels, SIZE, 32, 4), TRANSPARENT, "above");
}

#[test]
fn an_inverted_mask_hides_what_it_would_have_kept() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[
                ("shape", ParamValue::Choice(0)),
                ("size", ParamValue::point(Vec2::splat(0.5))),
                ("invert", ParamValue::Bool(true)),
            ],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 32, 32), TRANSPARENT, "inside is now hidden");
    assert_close(pixel(&pixels, SIZE, 2, 2), RED, "outside is now kept");
}

#[test]
fn an_ellipse_cuts_the_corners_a_rectangle_keeps() {
    let source = solid(SIZE, RED);
    let ellipse = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[("shape", ParamValue::Choice(1)), ("size", ParamValue::point(Vec2::splat(1.0)))],
        )],
    );
    assert_close(pixel(&ellipse, SIZE, 32, 32), RED, "the middle is inside either shape");
    assert_close(pixel(&ellipse, SIZE, 2, 2), TRANSPARENT, "the corner is outside an ellipse");

    let rectangle = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[("shape", ParamValue::Choice(0)), ("size", ParamValue::point(Vec2::splat(1.0)))],
        )],
    );
    assert_close(pixel(&rectangle, SIZE, 2, 2), RED, "and inside a rectangle");
}

#[test]
fn a_mask_at_half_opacity_half_hides_what_it_covers() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[
                ("shape", ParamValue::Choice(0)),
                ("size", ParamValue::point(Vec2::splat(0.5))),
                ("opacity", ParamValue::scalar(0.5)),
            ],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 32, 32), RED, "inside is untouched whatever the opacity");
    assert_close(pixel(&pixels, SIZE, 2, 2), [128, 0, 0, 128], "outside is half hidden");
}

#[test]
fn a_feathered_edge_ramps_instead_of_stepping() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::SHAPE_MASK,
            &[
                ("shape", ParamValue::Choice(0)),
                ("size", ParamValue::point(Vec2::splat(0.5))),
                ("feather", ParamValue::scalar(16.0)),
            ],
        )],
    );

    // Across the feathered edge at x=16, coverage climbs monotonically.
    let alphas: Vec<u8> = (8..25).map(|x| pixel(&pixels, SIZE, x, 32)[3]).collect();
    assert!(alphas[0] < 40, "outside the ramp: {alphas:?}");
    assert!(*alphas.last().unwrap() > 215, "inside it: {alphas:?}");
    assert!(alphas.windows(2).all(|w| w[1] >= w[0]), "not monotonic: {alphas:?}");
}

#[test]
fn a_luma_key_turns_the_dark_parts_transparent() {
    let source = frame(SIZE, |x, _| if x < 32 { [20, 20, 20, 255] } else { WHITE });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::LUMA_KEY,
            &[("threshold", ParamValue::scalar(0.3)), ("softness", ParamValue::scalar(0.1))],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 8, 32), TRANSPARENT, "the dark half is keyed out");
    assert_close(pixel(&pixels, SIZE, 56, 32), WHITE, "the bright half is kept");
}

#[test]
fn an_inverted_luma_key_keeps_the_dark_parts_instead() {
    let source = frame(SIZE, |x, _| if x < 32 { [20, 20, 20, 255] } else { WHITE });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::LUMA_KEY,
            &[
                ("threshold", ParamValue::scalar(0.3)),
                ("softness", ParamValue::scalar(0.1)),
                ("invert", ParamValue::Bool(true)),
            ],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 8, 32), [20, 20, 20, 255], "the dark half is kept");
    assert_close(pixel(&pixels, SIZE, 56, 32), TRANSPARENT, "the bright half is keyed out");
}

// --- Chains ------------------------------------------------------------------

#[test]
fn each_pass_reads_what_the_one_before_it_wrote() {
    // Mask first, then brighten. If the second pass read the original frame
    // instead of the first pass's output, the mask would simply be gone; if it
    // read it as straight colour, the masked-out area would come back as a
    // brightened black.
    let source = solid(SIZE, [64, 64, 64, 255]);
    let pixels = run(
        SIZE,
        &source,
        &[
            effect(
                kinds::SHAPE_MASK,
                &[
                    ("shape", ParamValue::Choice(0)),
                    ("size", ParamValue::point(Vec2::splat(0.5))),
                ],
            ),
            effect(kinds::COLOR_ADJUST, &[("exposure", ParamValue::scalar(1.0))]),
        ],
    );
    assert_close(pixel(&pixels, SIZE, 32, 32), [128, 128, 128, 255], "kept and brightened");
    assert_close(pixel(&pixels, SIZE, 2, 2), TRANSPARENT, "still masked out");
}

#[test]
fn order_is_the_chain() {
    // Blur then mask, and mask then blur, on the same picture. Masking last
    // cuts a hard edge through an already soft picture; masking first leaves an
    // edge for the blur to soften. The difference is exactly why reordering a
    // chain is an edit rather than a rearrangement of a list.
    let source = solid(SIZE, RED);
    let blur = effect(
        kinds::GAUSSIAN_BLUR,
        &[("radius", ParamValue::scalar(6.0)), ("direction", ParamValue::Choice(1))],
    );

    let mask_last = run(SIZE, &source, &[blur.clone(), mask_half()]);
    let mask_first = run(SIZE, &source, &[mask_half(), blur]);

    // The mask's left edge sits at x = 16. Two pixels outside it:
    let hard = pixel(&mask_last, SIZE, 12, 32);
    let soft = pixel(&mask_first, SIZE, 12, 32);
    assert_close(hard, TRANSPARENT, "masked last, the edge is where the mask put it");
    assert!(soft[3] > 20, "masked first, the blur carried it outwards: {soft:?}");
}

fn mask_half() -> EffectState {
    effect(
        kinds::SHAPE_MASK,
        &[("shape", ParamValue::Choice(0)), ("size", ParamValue::point(Vec2::splat(0.5)))],
    )
}

// --- Grading -----------------------------------------------------------------

const GREY: [u8; 4] = [128, 128, 128, 255];

fn wheel(r: f64, g: f64, b: f64) -> ParamValue {
    ParamValue::color(ve_core::Rgba::new(r, g, b, 1.0))
}

#[test]
fn lifting_the_shadows_moves_black_and_leaves_white_where_it_was() {
    // That is the whole point of a lift as opposed to an exposure: it is the
    // bottom of the range being dragged up, with the top pinned. A grade that
    // moved white too would be a gain wearing a lift's label.
    let source = frame(SIZE, |x, _| if x < 32 { BLACK } else { WHITE });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::THREE_WAY, &[("shadow_level", ParamValue::scalar(1.0))])],
    );
    // A full shadow level lifts black a quarter of the way up the range.
    let lifted = (0.25_f64 * 255.0).round() as u8;
    assert_close(pixel(&pixels, SIZE, 16, 32), [lifted, lifted, lifted, 255], "black lifted");
    assert_close(pixel(&pixels, SIZE, 48, 32), WHITE, "white pinned");
}

#[test]
fn raising_the_highlights_moves_white_and_leaves_black_where_it_was() {
    let source = frame(SIZE, |x, _| if x < 32 { BLACK } else { GREY });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::THREE_WAY, &[("highlight_level", ParamValue::scalar(1.0))])],
    );
    // Half a stop per unit, so a full level is a gain of √2. Measured on grey
    // rather than on white, which would clip and prove nothing.
    let gained = (128.0 * 2.0_f64.sqrt()).round() as u8;
    assert_close(pixel(&pixels, SIZE, 16, 32), BLACK, "black pinned");
    assert_close(pixel(&pixels, SIZE, 48, 32), [gained, gained, gained, 255], "grey gained");
}

#[test]
fn the_midtones_bend_without_moving_either_end() {
    let source = frame(SIZE, |x, _| match x / 16 {
        0 => BLACK,
        1 => GREY,
        _ => WHITE,
    });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::THREE_WAY, &[("midtone_level", ParamValue::scalar(1.0))])],
    );
    assert_close(pixel(&pixels, SIZE, 8, 32), BLACK, "black is a fixed point of a power");
    assert_close(pixel(&pixels, SIZE, 40, 32), WHITE, "and so is white");

    // The exponent is 2^-0.5, which brightens: 0.502^0.707 is about 0.617.
    let bent = ((128.0_f64 / 255.0).powf(2.0_f64.powf(-0.5)) * 255.0).round() as u8;
    assert_close(pixel(&pixels, SIZE, 24, 32), [bent, bent, bent, 255], "midtones bent");
    assert!(bent > 128, "a positive midtone level brightens");
}

#[test]
fn a_shadow_wheel_colours_the_darks_without_touching_the_whites() {
    // A colourist's first move: warm the shadows, leave the highlights neutral.
    // Measured on a dark grey rather than on black, because a wheel pushes one
    // channel down as well as another up and black has nowhere lower to go.
    let dark = [64, 64, 64, 255];
    let source = frame(SIZE, |x, _| if x < 32 { dark } else { WHITE });
    let pixels = run(
        SIZE,
        &source,
        // Red above mid grey, blue below: warm.
        &[effect(kinds::THREE_WAY, &[("shadows", wheel(0.75, 0.5, 0.25))])],
    );

    // Half a deflection each way, so a lift of ±0.125 — and a slope of
    // `gain − lift` that follows it, which is what keeps white pinned.
    let lift = [0.125_f64, 0.0, -0.125];
    let expected: Vec<u8> = (0..3)
        .map(|c| (((64.0 / 255.0) * (1.0 - lift[c]) + lift[c]) * 255.0).round() as u8)
        .collect();
    let shadow = pixel(&pixels, SIZE, 16, 32);
    assert_close(shadow, [expected[0], expected[1], expected[2], 255], "warmed darks");
    assert!(shadow[0] > shadow[1] && shadow[1] > shadow[2], "warm: {shadow:?}");
    assert_close(pixel(&pixels, SIZE, 48, 32), WHITE, "white is untouched by a lift");
}

#[test]
fn a_grade_and_its_opposite_cancel() {
    // Half a stop up and half a stop down are reciprocal by construction, which
    // is what makes the sliders usable: pulling one back to where it was gets
    // the picture back rather than leaving it slightly flatter than it started.
    let source = solid(SIZE, GREY);
    let pixels = run(
        SIZE,
        &source,
        &[
            effect(kinds::THREE_WAY, &[("midtone_level", ParamValue::scalar(0.5))]),
            effect(kinds::THREE_WAY, &[("midtone_level", ParamValue::scalar(-0.5))]),
        ],
    );
    assert_close(pixel(&pixels, SIZE, 32, 32), GREY, "back where it started");
}

#[test]
fn a_grade_does_not_depend_on_coverage() {
    // The same colour at two coverages. A decoded frame carries straight
    // colour, so both sides are the same number and only the alpha differs.
    let source = frame(SIZE, |x, _| if x < 32 { [64, 64, 64, 255] } else { [64, 64, 64, 128] });
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::THREE_WAY, &[("highlight_level", ParamValue::scalar(1.0))])],
    );
    // Both pixels are the same colour at different coverage, so both are gained
    // by the same factor — half a stop — and written back premultiplied.
    let gained = 64.0 * 2.0_f64.sqrt();
    assert_close(
        pixel(&pixels, SIZE, 16, 32),
        [gained.round() as u8, gained.round() as u8, gained.round() as u8, 255],
        "opaque",
    );
    let half = (gained * 0.5).round() as u8;
    assert_close(pixel(&pixels, SIZE, 48, 32), [half, half, half, 128], "half covered");
}

// --- White balance -----------------------------------------------------------

#[test]
fn warming_trades_blue_for_red_and_keeps_the_brightness() {
    let source = solid(SIZE, GREY);
    let pixels = run(
        SIZE,
        &source,
        &[effect(kinds::WHITE_BALANCE, &[("temperature", ParamValue::scalar(1.0))])],
    );
    let warm = pixel(&pixels, SIZE, 32, 32);
    assert!(warm[0] > 128, "red is raised: {warm:?}");
    assert!(warm[2] < 128, "blue is lowered: {warm:?}");

    // Rec. 709 luma is what is held constant, so the grey keeps its weight.
    let luma = 0.2126 * warm[0] as f64 + 0.7152 * warm[1] as f64 + 0.0722 * warm[2] as f64;
    assert!((luma - 128.0).abs() < 3.0, "luma is preserved: {luma} from {warm:?}");
}

#[test]
fn cooling_is_the_inverse_of_warming() {
    let source = solid(SIZE, GREY);
    let warmed = run(
        SIZE,
        &source,
        &[
            effect(kinds::WHITE_BALANCE, &[("temperature", ParamValue::scalar(0.5))]),
            effect(kinds::WHITE_BALANCE, &[("temperature", ParamValue::scalar(-0.5))]),
        ],
    );
    assert_close(pixel(&warmed, SIZE, 32, 32), GREY, "warmed and cooled back");
}

#[test]
fn a_tint_moves_green_against_magenta() {
    let source = solid(SIZE, GREY);
    let green = run(
        SIZE,
        &source,
        &[effect(kinds::WHITE_BALANCE, &[("tint", ParamValue::scalar(-1.0))])],
    );
    let g = pixel(&green, SIZE, 32, 32);
    assert!(g[1] > 128, "green is raised: {g:?}");
    assert!(g[0] < 128 && g[2] < 128, "red and blue fall together: {g:?}");
    assert_eq!(g[0], g[2], "a tint does not disturb the warm/cool axis");
}

// --- HSL secondary -----------------------------------------------------------

#[test]
fn a_secondary_grades_the_hue_it_selected_and_nothing_else() {
    // Red on the left, blue on the right, with the band centred on red.
    let blue = [0, 0, 255, 255];
    let source = frame(SIZE, |x, _| if x < 32 { RED } else { blue });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::HSL_SECONDARY,
            &[
                ("hue", ParamValue::scalar(0.0)),
                ("hue_width", ParamValue::scalar(20.0)),
                ("softness", ParamValue::scalar(5.0)),
                ("saturation_scale", ParamValue::scalar(0.0)),
            ],
        )],
    );
    // Selected: drained to its own lightness, which for full red is half grey.
    let drained = pixel(&pixels, SIZE, 16, 32);
    assert_eq!(drained[0], drained[1], "the selected red is now neutral: {drained:?}");
    assert_eq!(drained[1], drained[2], "the selected red is now neutral: {drained:?}");
    // Not selected: untouched.
    assert_close(pixel(&pixels, SIZE, 48, 32), blue, "blue is outside the band");
}

#[test]
fn a_band_centred_on_red_reaches_across_the_seam() {
    // Hue wraps, and red sits exactly on the wrap. A band centred on 0 that
    // only reached upwards would select orange and not magenta.
    let magenta = [255, 0, 128, 255];
    let orange = [255, 160, 0, 255];
    let source = frame(SIZE, |x, _| if x < 32 { magenta } else { orange });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::HSL_SECONDARY,
            &[
                ("hue", ParamValue::scalar(0.0)),
                ("hue_width", ParamValue::scalar(45.0)),
                ("softness", ParamValue::scalar(1.0)),
                ("show_matte", ParamValue::Bool(true)),
            ],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 16, 32), WHITE, "magenta is inside the band");
    assert_close(pixel(&pixels, SIZE, 48, 32), WHITE, "and so is orange");
}

#[test]
fn the_saturation_floor_keeps_greys_out_of_the_key() {
    // Without it, a band centred anywhere selects every neutral in the frame,
    // because grey's hue is whatever the arithmetic happened to produce.
    let source = frame(SIZE, |x, _| if x < 32 { RED } else { GREY });
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::HSL_SECONDARY,
            &[
                ("hue", ParamValue::scalar(0.0)),
                ("hue_width", ParamValue::scalar(180.0)),
                ("saturation_floor", ParamValue::scalar(0.3)),
                ("show_matte", ParamValue::Bool(true)),
            ],
        )],
    );
    assert_close(pixel(&pixels, SIZE, 16, 32), WHITE, "saturated red is selected");
    assert_close(pixel(&pixels, SIZE, 48, 32), BLACK, "grey is not");
}

#[test]
fn shifting_a_hue_turns_it_without_changing_how_bright_it_is() {
    let source = solid(SIZE, RED);
    let pixels = run(
        SIZE,
        &source,
        &[effect(
            kinds::HSL_SECONDARY,
            &[
                ("hue", ParamValue::scalar(0.0)),
                ("hue_width", ParamValue::scalar(30.0)),
                ("hue_shift", ParamValue::scalar(120.0)),
            ],
        )],
    );
    // A third of a turn from red is green, at the same saturation and lightness.
    assert_close(pixel(&pixels, SIZE, 32, 32), [0, 255, 0, 255], "red turned to green");
}

#[test]
fn a_pass_in_linear_light_differs_from_the_same_pass_encoded() {
    // The colour space is the attachment's, exactly as it is for compositing:
    // the hardware decodes on sample and encodes on store, so the arithmetic in
    // between is on light rather than on codes. Doubling mid grey is the
    // clearest case — half the light is not half the code.
    let source = solid(SIZE, [128, 128, 128, 255]);
    let adjust = effect(kinds::COLOR_ADJUST, &[("exposure", ParamValue::scalar(-1.0))]);

    let perceptual =
        run_in(ColorSpace::Perceptual, SIZE, &source, std::slice::from_ref(&adjust));
    let linear = run_in(ColorSpace::Linear, SIZE, &source, &[adjust]);
    let a = pixel(&perceptual, SIZE, 32, 32)[0];
    let b = pixel(&linear, SIZE, 32, 32)[0];
    assert_close([a, 0, 0, 0], [64, 0, 0, 0], "halving the code");
    assert!(b > a + 10, "halving the light lands brighter: {b} vs {a}");
}

#[test]
fn the_cache_key_changes_with_every_parameter_that_reaches_the_shader() {
    let size = SIZE;
    let space = ColorSpace::Perceptual;
    let source = ve_render::TextureId::from_content(7);

    let key_for = |radius: f64| {
        let passes = chain_passes(
            &[effect(kinds::GAUSSIAN_BLUR, &[("radius", ParamValue::scalar(radius))])],
            size,
        );
        CompositeKey::of_effect(source, size, space, &passes[0])
    };
    assert_eq!(key_for(8.0), key_for(8.0), "the same pass is the same key");
    assert_ne!(key_for(8.0), key_for(9.0), "a changed radius is a changed picture");

    // And the two axes of one blur are different pictures of the same source.
    let both = chain_passes(&[effect(kinds::GAUSSIAN_BLUR, &[])], size);
    assert_ne!(
        CompositeKey::of_effect(source, size, space, &both[0]),
        CompositeKey::of_effect(source, size, space, &both[1]),
    );
}
