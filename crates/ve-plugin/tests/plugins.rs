use ve_core::{builtin_registry, EffectId};
use ve_plugin::{load_dir, Manifest, PluginError};

const GOOD: &str = r#"{
  "api_version": 1, "id": "acme", "name": "Acme", "version": "1.0.0",
  "effects": [{
    "kind": "acme.glow", "name": "Glow", "category": "stylize", "summary": "Soft glow",
    "params": [
      {"key": "amount", "label": "Amount", "type": "scalar", "default": 0.5, "min": 0, "max": 1},
      {"key": "tint", "label": "Tint", "type": "color", "default": [1,1,1,1]},
      {"key": "mode", "label": "Mode", "type": "choice", "default": 0, "options": ["add","screen"]}
    ]
  }]
}"#;

#[test]
fn registers_effects_with_declared_params() {
    let m = Manifest::parse(GOOD).unwrap();
    let mut reg = builtin_registry().clone();
    let before = reg.len();
    m.register(&mut reg).unwrap();
    assert_eq!(reg.len(), before + 1);
    let fx = reg.instantiate("acme.glow", EffectId::from_raw(1)).unwrap();
    assert_eq!(fx.params.len(), 3);
}

#[test]
fn rejects_bad_manifests() {
    let ns = GOOD.replace("acme.glow", "other.glow");
    assert!(matches!(Manifest::parse(&ns), Err(PluginError::NotNamespaced { .. })));
    let ver = GOOD.replace("\"api_version\": 1", "\"api_version\": 9");
    assert!(matches!(Manifest::parse(&ver), Err(PluginError::ApiVersion { found: 9 })));
    let range = GOOD.replace("\"default\": 0.5", "\"default\": 5");
    assert!(matches!(Manifest::parse(&range), Err(PluginError::BadEffect { .. })));
}

#[test]
fn duplicate_kind_registers_nothing() {
    let m = Manifest::parse(GOOD).unwrap();
    let mut reg = builtin_registry().clone();
    m.register(&mut reg).unwrap();
    let n = reg.len();
    assert!(matches!(m.register(&mut reg), Err(PluginError::Duplicate(_))));
    assert_eq!(reg.len(), n);
}

#[test]
fn load_dir_skips_broken_plugins() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("a")).unwrap();
    std::fs::write(root.path().join("a/plugin.json"), GOOD).unwrap();
    std::fs::create_dir(root.path().join("b")).unwrap();
    std::fs::write(root.path().join("b/plugin.json"), "{").unwrap();
    let mut reg = builtin_registry().clone();
    let out = load_dir(root.path(), &mut reg);
    assert_eq!(out.len(), 2);
    assert!(out[0].1.is_ok());
    assert!(out[1].1.is_err());
    assert!(reg.contains("acme.glow"));
}

#[test]
fn an_installed_registry_is_the_one_the_editor_resolves_against() {
    use ve_core::{active_registry, install_registry};

    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("tint");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), GOOD).unwrap();

    let mut reg = builtin_registry().clone();
    let loaded = ve_plugin::load_dir(dir.path(), &mut reg);
    assert!(loaded[0].1.is_ok(), "{:?}", loaded[0].1);

    install_registry(reg).expect("first install wins");
    assert!(active_registry().get("acme.glow").is_some());
    assert!(builtin_registry().get("acme.glow").is_none());
}

#[test]
fn a_shader_is_read_from_beside_the_manifest_and_must_stay_inside() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("p");
    std::fs::create_dir(&dir).unwrap();
    let with =
        |path: &str| GOOD.replace("\"summary\"", &format!("\"shader\": {path:?}, \"summary\""));

    std::fs::write(dir.join("plugin.json"), with("glow.wgsl")).unwrap();
    assert!(Manifest::load(&dir).is_err(), "a missing shader file is an error");

    std::fs::write(
        dir.join("glow.wgsl"),
        "fn effect(uv: vec2<f32>, c: vec4<f32>) -> vec4<f32> { return c; }",
    )
    .unwrap();
    let m = Manifest::load(&dir).unwrap();
    let mut reg = builtin_registry().clone();
    m.register(&mut reg).unwrap();
    assert!(reg.get("acme.glow").unwrap().shader.is_some());

    std::fs::write(dir.join("plugin.json"), with("../outside.wgsl")).unwrap();
    assert!(Manifest::load(&dir).is_err());

    // Parsed without a directory, a shader cannot have been read, so it will not register.
    let parsed = Manifest::parse(&with("glow.wgsl")).unwrap();
    assert!(parsed.register(&mut builtin_registry().clone()).is_err());

    // A host's check can refuse the plugin.
    std::fs::write(dir.join("plugin.json"), with("glow.wgsl")).unwrap();
    let mut reg = builtin_registry().clone();
    let out = ve_plugin::load_dir_checked(root.path(), &mut reg, &|_| Err("no".into()));
    assert!(out[0].1.is_err());
    assert!(!reg.contains("acme.glow"));
}
