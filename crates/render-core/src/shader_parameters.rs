use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Стабильный id shader parameter-а в renderer-neutral live settings contract.
///
/// Id хранится строкой, потому что будущие shader controls будут добавляться без
/// изменения enum-а. Значение всё равно типизируется через descriptor/value ниже.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShaderParameterId(String);

impl ShaderParameterId {
    /// Создаёт новый стабильный id shader parameter-а.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Возвращает id без аллокации для diagnostics и metadata mapping.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Стабильный id enum option-а внутри shader parameter descriptor-а.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShaderParameterOptionId(String);

impl ShaderParameterOptionId {
    /// Создаёт новый стабильный id option-а.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Возвращает id option-а без аллокации.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Тип значения shader parameter-а; UI и runtime не угадывают его из строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderParameterValueType {
    /// Boolean shader switch.
    Bool,

    /// Один scalar `f32`.
    Float,

    /// RGB/vector-like triplet из трёх `f32`.
    Float3,

    /// Одно значение из стабильного списка option ids.
    Enum,
}

/// Числовой диапазон shader parameter-а.
///
/// Диапазон нейтральный: здесь нет slider/egui-представления, только контракт
/// значений, которые adapter может безопасно принять.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderNumericRange {
    /// Минимальное допустимое значение включительно.
    pub min: f32,

    /// Максимальное допустимое значение включительно.
    pub max: f32,

    /// Optional step для UI/metadata; adapter не обязан квантовать значение.
    pub step: Option<f32>,
}

impl ShaderNumericRange {
    /// Проверяет один `f32` на конечность и попадание в диапазон.
    #[must_use]
    pub fn contains_float(self, value: f32) -> bool {
        value.is_finite() && value >= self.min && value <= self.max
    }

    /// Проверяет `Float3`, применяя один диапазон ко всем каналам.
    #[must_use]
    pub fn contains_float3(self, values: [f32; 3]) -> bool {
        values
            .into_iter()
            .all(|channel_value| self.contains_float(channel_value))
    }
}

/// Типизированное значение shader parameter-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderParameterValue {
    /// Boolean shader switch.
    Bool(bool),

    /// Один scalar `f32`.
    Float(f32),

    /// Три `f32`, например RGB gain/offset-like параметр.
    Float3([f32; 3]),

    /// Стабильный enum option id.
    Enum(ShaderParameterOptionId),
}

impl ShaderParameterValue {
    /// Возвращает тип значения без доступа к descriptor registry.
    #[must_use]
    pub const fn value_type(&self) -> ShaderParameterValueType {
        match self {
            Self::Bool(_) => ShaderParameterValueType::Bool,
            Self::Float(_) => ShaderParameterValueType::Float,
            Self::Float3(_) => ShaderParameterValueType::Float3,
            Self::Enum(_) => ShaderParameterValueType::Enum,
        }
    }
}

/// Descriptor одного shader parameter-а.
///
/// Это schema для live shader controls: stable id, тип значения, optional range и
/// default. Такой контракт не требует `HashMap<String, f32>` и остаётся
/// расширяемым для будущих shader passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameterDescriptor {
    /// Stable id parameter-а.
    pub id: ShaderParameterId,

    /// Ожидаемый тип значения.
    pub value_type: ShaderParameterValueType,

    /// Optional numeric range только для `Float`/`Float3`.
    pub range: Option<ShaderNumericRange>,

    /// Default value, который должен соответствовать `value_type` и `range`.
    pub default_value: ShaderParameterValue,
}

impl ShaderParameterDescriptor {
    /// Создаёт descriptor без backend-specific state.
    #[must_use]
    pub fn new(
        id: ShaderParameterId,
        value_type: ShaderParameterValueType,
        range: Option<ShaderNumericRange>,
        default_value: ShaderParameterValue,
    ) -> Self {
        Self {
            id,
            value_type,
            range,
            default_value,
        }
    }

    /// Проверяет значение по типу и optional numeric range.
    #[must_use]
    pub fn accepts_value(&self, value: &ShaderParameterValue) -> bool {
        if value.value_type() != self.value_type {
            return false;
        }

        match (self.range, value) {
            (Some(range), ShaderParameterValue::Float(value)) => range.contains_float(*value),
            (Some(range), ShaderParameterValue::Float3(values)) => range.contains_float3(*values),
            (Some(_), ShaderParameterValue::Bool(_) | ShaderParameterValue::Enum(_)) => false,
            (None, _) => true,
        }
    }

    /// Проверяет, что default value descriptor-а сам валиден.
    #[must_use]
    pub fn default_value_is_valid(&self) -> bool {
        self.accepts_value(&self.default_value)
    }
}

/// Одно текущее значение shader parameter-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameter {
    /// Stable id parameter-а.
    pub id: ShaderParameterId,

    /// Типизированное значение parameter-а.
    pub value: ShaderParameterValue,
}

impl ShaderParameter {
    /// Создаёт parameter value pair.
    #[must_use]
    pub fn new(id: ShaderParameterId, value: ShaderParameterValue) -> Self {
        Self { id, value }
    }
}

/// Набор shader parameter values без backend-specific storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameterSet {
    /// Ordered values. Это не `HashMap<String, f32>`: каждый value несёт свой тип.
    pub parameters: Vec<ShaderParameter>,
}

impl ShaderParameterSet {
    /// Возвращает пустой набор shader parameters.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            parameters: Vec::new(),
        }
    }

    /// Проверяет, что shader parameters отсутствуют.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    /// Ищет parameter по stable id.
    #[must_use]
    pub fn get(&self, id: &ShaderParameterId) -> Option<&ShaderParameter> {
        self.parameters.iter().find(|parameter| parameter.id == *id)
    }

    /// Возвращает stable ids shader parameters, отличающихся от baseline.
    #[must_use]
    pub fn changed_parameter_ids_from(&self, baseline: &Self) -> Vec<ShaderParameterId> {
        let mut candidate_ids = BTreeSet::new();

        for parameter in &self.parameters {
            candidate_ids.insert(parameter.id.clone());
        }

        for parameter in &baseline.parameters {
            candidate_ids.insert(parameter.id.clone());
        }

        candidate_ids
            .into_iter()
            .filter(|id| {
                let current_value = self.get(id).map(|parameter| &parameter.value);
                let baseline_value = baseline.get(id).map(|parameter| &parameter.value);

                current_value != baseline_value
            })
            .collect()
    }
}
#[cfg(test)]
#[path = "tests/shader_parameters.rs"]
mod tests;
