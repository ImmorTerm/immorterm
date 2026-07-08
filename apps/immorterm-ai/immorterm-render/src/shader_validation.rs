//! Offline WGSL shader validation via naga.
//!
//! Parses and validates all WGSL shaders at test time to catch syntax errors,
//! type mismatches, binding conflicts, and other semantic issues before they
//! reach the GPU driver.
//!
//! NOTE: naga 24 intentionally disables fragment-stage uniformity analysis
//! (`DISABLE_UNIFORMITY_REQ_FOR_FRAGMENT_STAGE = true`), so `textureSample`
//! in non-uniform control flow will NOT be flagged here. Dawn (Electron's
//! WebGPU) does enforce this at runtime. For that class of bug, always use
//! `textureSampleLevel` with explicit LOD inside conditional blocks.

use naga::front::wgsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};

/// Shader sources — identical `include_str!` paths used by pipeline.rs and images.rs.
const SHADERS: &[(&str, &str)] = &[
    ("bg.wgsl", include_str!("shaders/bg.wgsl")),
    ("glyph.wgsl", include_str!("shaders/glyph.wgsl")),
    ("decor.wgsl", include_str!("shaders/decor.wgsl")),
    ("image.wgsl", include_str!("shaders/image.wgsl")),
];

fn parse_shader(name: &str, source: &str) -> naga::Module {
    match wgsl::parse_str(source) {
        Ok(module) => module,
        Err(e) => {
            let diagnostic = e.emit_to_string(source);
            panic!("\n=== WGSL parse error in {name} ===\n{diagnostic}\n");
        }
    }
}

fn validate_module(name: &str, module: &naga::Module) {
    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::default());
    if let Err(e) = validator.validate(module) {
        panic!("\n=== WGSL validation error in {name} ===\n{e:?}\n");
    }
}

#[test]
fn all_shaders_parse() {
    for &(name, source) in SHADERS {
        parse_shader(name, source);
    }
}

#[test]
fn all_shaders_validate() {
    for &(name, source) in SHADERS {
        let module = parse_shader(name, source);
        validate_module(name, &module);
    }
}

/// Verify each shader has the expected entry points (`vs_main`, `fs_main`).
/// Catches accidental renames that would cause runtime pipeline creation failures.
#[test]
fn all_shaders_have_entry_points() {
    for &(name, source) in SHADERS {
        let module = parse_shader(name, source);
        let entry_names: Vec<&str> = module.entry_points.iter().map(|ep| ep.name.as_str()).collect();

        assert!(
            entry_names.contains(&"vs_main"),
            "{name} is missing vs_main entry point (found: {entry_names:?})"
        );
        assert!(
            entry_names.contains(&"fs_main"),
            "{name} is missing fs_main entry point (found: {entry_names:?})"
        );
    }
}

#[test]
fn all_shader_sources_nonempty() {
    for &(name, source) in SHADERS {
        assert!(
            !source.trim().is_empty(),
            "Shader {name} resolved to empty source"
        );
    }
}

/// Ban `textureSample(` — always use `textureSampleLevel` with explicit LOD.
///
/// `textureSample` computes implicit derivatives from neighboring fragment
/// invocations, requiring uniform control flow. Dawn (Electron's WebGPU)
/// enforces this strictly and rejects the shader, while Chrome allows it.
/// Since all our atlases have mip_level_count=1, there is never a reason
/// to use implicit LOD. This test would have caught the black-screen bug.
#[test]
fn no_implicit_lod_texture_sampling() {
    for &(name, source) in SHADERS {
        // Match "textureSample(" but not "textureSampleLevel(",
        // "textureSampleGrad(", "textureSampleCompare(", etc.
        for (line_num, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            // Find all occurrences of "textureSample(" that aren't followed by
            // an uppercase letter (which would make it a different function).
            let mut search_from = 0;
            while let Some(pos) = trimmed[search_from..].find("textureSample(") {
                let abs_pos = search_from + pos;
                // Check it's not part of textureSampleLevel, textureSampleGrad, etc.
                let before_paren = &trimmed[abs_pos..abs_pos + "textureSample(".len()];
                if before_paren == "textureSample(" {
                    panic!(
                        "\n{name}:{} — found `textureSample(` (implicit LOD).\n\
                         Use `textureSampleLevel(..., 0.0)` instead.\n\
                         Implicit LOD requires uniform control flow which Dawn \
                         (Electron) enforces strictly.\n\
                         Line: {}\n",
                        line_num + 1,
                        trimmed,
                    );
                }
                search_from = abs_pos + 1;
            }
        }
    }
}
