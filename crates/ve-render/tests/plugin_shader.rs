use ve_core::{
    active_registry, builtin_registry, install_registry, EffectCategory, EffectDescriptor,
    EffectId, ParamDescriptor, ParamKind, Size, Vec2,
};
use ve_render::{passes_for, validate_plugin_shader, Program};

const GOOD: &str =
    "fn effect(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> { return c * fx.p0.x; }";

#[test]
fn a_valid_shader_passes_and_a_broken_one_is_explained() {
    assert!(validate_plugin_shader(GOOD).is_ok());
    assert!(validate_plugin_shader("fn effect( {").is_err());
    // Parses, but there is no `effect` for the entry point to call.
    assert!(validate_plugin_shader("fn other() {}").is_err());
}

#[test]
fn an_installed_shader_effect_becomes_a_plugin_pass_with_packed_params() {
    let mut reg = builtin_registry().clone();
    reg.register(
        EffectDescriptor::new("acme.dim", "Dim", EffectCategory::Color, "")
            .with_param(ParamDescriptor::new(
                "gain",
                "Gain",
                ParamKind::scalar(0.5, 0.0, 1.0),
                "",
            ))
            .with_param(ParamDescriptor::new(
                "at",
                "At",
                ParamKind::point(Vec2::new(3.0, 4.0), 0.0, 10.0),
                "",
            ))
            .with_shader(GOOD),
    );
    assert!(install_registry(reg).is_ok(), "first install");

    let state = active_registry()
        .instantiate("acme.dim", EffectId::from_raw(1))
        .unwrap()
        .evaluate(ve_time::Ticks::ZERO, active_registry().get("acme.dim"));
    let passes = passes_for(&state, Size::new(1920, 1080));
    assert_eq!(passes.len(), 1);
    assert_eq!(passes[0].program(), Program::Plugin);
    assert_eq!(passes[0].plugin_kind(), Some("acme.dim"));
}
