use super::*;
#[test]
fn shader_parameter_descriptor_keeps_typed_value_contract() {
    let descriptor = ShaderParameterDescriptor::new(
        ShaderParameterId::new("render.shader.preview_strength"),
        ShaderParameterValueType::Float,
        Some(ShaderNumericRange {
            min: 0.0,
            max: 1.0,
            step: Some(0.01),
        }),
        ShaderParameterValue::Float(0.5),
    );

    assert!(descriptor.default_value_is_valid());
    assert!(descriptor.accepts_value(&ShaderParameterValue::Float(1.0)));
    assert!(!descriptor.accepts_value(&ShaderParameterValue::Float(1.5)));
    assert!(!descriptor.accepts_value(&ShaderParameterValue::Float3([0.5, 0.5, 0.5])));
}
