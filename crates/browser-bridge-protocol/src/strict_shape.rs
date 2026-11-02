//! Строгая форма JSON-значений протокола (сессия 07, решение 86).
//!
//! # Корень дефекта
//!
//! Производный (`#[derive(Deserialize)]`) разбор serde рассчитан на любые
//! форматы и поэтому принимает одни и те же данные в нескольких формах:
//! - структуру — и из объекта, и из массива полей по порядку
//!   (`"protocol":[1,0]`);
//! - перечисление с полем-меткой (`tag = "kind"`) — тоже из массива, где
//!   первый элемент — метка (`"end":["at",5]`);
//! - имя варианта — и строкой, и **номером варианта** (`"kind":0`); номер
//!   вне предела давал `number_out_of_range` вместо ошибки формы;
//! - метку-перечисление (`"state":"playing"`) — ещё и внешней формой serde
//!   `{"playing":null}`.
//!
//! Все сообщения протокола помечены полем `type`, а serde разбирает такие
//! перечисления в два прохода: сначала складывает сообщение в свой
//! внутренний буфер, потом читает поля из буфера. Читатель буфера ещё
//! терпимее читателя JSON (именно он понимает номер варианта), и обёртка
//! вокруг читателя до буфера не доживает. Поэтому строгость нельзя навесить
//! снаружи (на `serde_json::from_str`) — её должен нести **каждый тип сам**,
//! где бы он ни встретился.
//!
//! # Как закрыто
//!
//! Каждый тип протокола с производным разбором объявлен с
//! `#[serde(remote = "Self")]`: тогда serde кладёт свой разбор не в
//! `impl Deserialize`, а в обычную функцию типа. Настоящий `impl Deserialize`
//! пишут макросы этого модуля, и он сначала проверяет форму, а потом отдаёт
//! данные производному разбору:
//! - [`strict_object!`] — структуры и перечисления с полем-меткой: значение
//!   обязано быть JSON-объектом, а любое имя внутри него (ключ поля, значение
//!   метки `kind`/`type`) — строкой;
//! - [`strict_label!`] — перечисления-метки без полей: значение обязано быть
//!   JSON-строкой.
//!
//! Чужая форма отвергается ошибкой serde «invalid type …», которую
//! `crate::error::classify_json_error` переводит в `wrong_value_type` — тот же
//! вид, что у «строка вместо числа» (решение 86: значение не того типа).
//! Незнакомая метка в правильной форме, как и раньше, — `unknown_value`.
//!
//! # Чего не делать
//!
//! `#[serde(remote = "Self")]` оставляет у типа функцию `T::deserialize`,
//! написанную serde, — **нестрогую**. Явный вызов `ProtocolVersion::deserialize(…)`
//! по имени типа попадает в неё, а не в `Deserialize::deserialize`. Разбирайте
//! сообщения только через `crate::codec::decode_message` или обобщённо
//! (`serde_json::from_str::<T>`, `<T as Deserialize>::deserialize`).

use std::fmt;
use std::marker::PhantomData;

use serde::de::value::{MapAccessDeserializer, StrDeserializer};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, Visitor};

/// Разбор типа, который вывел serde (`#[serde(remote = "Self")]`).
///
/// Реализуют только макросы этого модуля. Вызывать его можно лишь после
/// проверки формы — сам по себе он нестрогий (см. документацию модуля).
pub(crate) trait DerivedDeserialize: Sized {
    /// Производный разбор serde без проверки формы.
    ///
    /// # Errors
    /// Ошибка производного разбора (поля, метки, значения).
    fn deserialize_derived<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
}

/// Разбирает тип, который в JSON обязан быть объектом.
///
/// Массив, строка, число и прочее отвергаются до производного разбора. Внутри
/// объекта имена (ключи и значения метки варианта) принимаются только строкой.
///
/// # Errors
/// «invalid type» для чужой формы или ошибка производного разбора.
pub(crate) fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DerivedDeserialize,
{
    // `deserialize_map` просит у читателя именно объект: из массива читатель
    // JSON и читатель буфера serde отвечают «invalid type: sequence».
    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

/// Разбирает перечисление-метку, которое в JSON обязано быть строкой.
///
/// # Errors
/// «invalid type» для чужой формы (число, объект) или «unknown variant» для
/// незнакомой строки.
pub(crate) fn deserialize_label<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DerivedDeserialize,
{
    // `deserialize_str` просит именно строку; число и объект приходят в
    // посетитель, который их не умеет принимать, — это «invalid type».
    deserializer.deserialize_str(LabelVisitor(PhantomData))
}

/// Принимает только JSON-объект и передаёт его производному разбору.
struct ObjectVisitor<T>(PhantomData<T>);

impl<'de, T: DerivedDeserialize> Visitor<'de> for ObjectVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
        // Производный разбор получает тот же объект, но имена в нём читаются
        // только из строк (метка `kind` числом — ошибка формы, а не номер).
        T::deserialize_derived(MapAccessDeserializer::new(TextNamesMap(map)))
    }
}

/// Принимает только JSON-строку и разбирает её производным разбором метки.
struct LabelVisitor<T>(PhantomData<T>);

impl<'de, T: DerivedDeserialize> Visitor<'de> for LabelVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a text label")
    }

    // `visit_string` и `visit_borrowed_str` по умолчанию приходят сюда же.
    fn visit_str<E: de::Error>(self, label: &str) -> Result<T, E> {
        // Читатель одной строки умеет отдать её только как строку, поэтому
        // производный разбор не увидит ни номера варианта, ни объекта.
        T::deserialize_derived(StrDeserializer::<E>::new(label))
    }
}

/// Объект, у которого каждое имя (ключ и метка варианта) читается только строкой.
///
/// Метку `kind`/`type` производный разбор читает как значение поля, поэтому
/// обёрнуты и ключи, и значения. Обёртка меняет только чтение имён; всё
/// остальное (числа, строки, вложенные объекты) проходит без изменений.
struct TextNamesMap<A>(A);

impl<'de, A: MapAccess<'de>> MapAccess<'de> for TextNamesMap<A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        self.0.next_key_seed(TextNamesSeed(seed))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.next_value_seed(TextNamesSeed(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

/// Передаёт разбору одного значения читатель, у которого имена — только строки.
struct TextNamesSeed<S>(S);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for TextNamesSeed<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.0.deserialize(TextNamesDeserializer(deserializer))
    }
}

/// Читатель-посредник: всё как у исходного, кроме чтения имени.
struct TextNamesDeserializer<D>(D);

/// Методы читателя, которые посредник передаёт исходному без изменений.
macro_rules! forward_to_inner_deserializer {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                self.0.$method(visitor)
            }
        )*
    };
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for TextNamesDeserializer<D> {
    type Error = D::Error;

    forward_to_inner_deserializer! {
        deserialize_any deserialize_bool
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64 deserialize_i128
        deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64 deserialize_u128
        deserialize_f32 deserialize_f64 deserialize_char deserialize_str deserialize_string
        deserialize_bytes deserialize_byte_buf deserialize_option deserialize_unit
        deserialize_seq deserialize_map deserialize_ignored_any
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.deserialize_enum(name, variants, visitor)
    }

    /// Единственное отличие: имя (поля или варианта) принимается только строкой.
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.0.deserialize_identifier(TextOnlyVisitor(visitor))
    }

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

/// Посетитель имени, который пропускает только строки.
///
/// Числа (номер варианта или поля), байты и всё прочее получают реализацию
/// по умолчанию из serde — ошибку «invalid type» с ожиданием исходного
/// посетителя.
struct TextOnlyVisitor<V>(V);

impl<'de, V: Visitor<'de>> Visitor<'de> for TextOnlyVisitor<V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.expecting(formatter)
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
        self.0.visit_str(name)
    }

    fn visit_borrowed_str<E: de::Error>(self, name: &'de str) -> Result<Self::Value, E> {
        self.0.visit_borrowed_str(name)
    }

    fn visit_string<E: de::Error>(self, name: String) -> Result<Self::Value, E> {
        self.0.visit_string(name)
    }
}

/// Общая часть макросов: запись — производная, производный разбор — через
/// функции, которые serde кладёт в тип при `#[serde(remote = "Self")]`.
///
/// Внутри `impl` путь `<$type>::serialize` / `<$type>::deserialize` находит
/// собственную функцию типа раньше метода типажа — это и есть производный код.
/// Если у типа забыть `#[serde(remote = "Self")]`, derive сам напишет
/// `impl Serialize`/`impl Deserialize`, и компиляция упадёт на повторной
/// реализации — молча строгость не потеряется.
macro_rules! derived_shape_impls {
    ($type:ty) => {
        impl $crate::strict_shape::DerivedDeserialize for $type {
            fn deserialize_derived<'de, D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                <$type>::deserialize(deserializer)
            }
        }

        impl ::serde::Serialize for $type {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                <$type>::serialize(self, serializer)
            }
        }
    };
}

/// Строгий разбор для структур и перечислений с полем-меткой: только JSON-объект.
///
/// Тип обязан быть объявлен с `#[derive(Serialize, Deserialize)]` и
/// `#[serde(remote = "Self")]`.
macro_rules! strict_object {
    ($($type:ty),+ $(,)?) => {
        $(
            $crate::strict_shape::derived_shape_impls!($type);

            impl<'de> ::serde::Deserialize<'de> for $type {
                fn deserialize<D: ::serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    $crate::strict_shape::deserialize_object(deserializer)
                }
            }
        )+
    };
}

/// Строгий разбор для перечислений-меток без полей: только JSON-строка.
///
/// Тип обязан быть объявлен с `#[derive(Serialize, Deserialize)]` и
/// `#[serde(remote = "Self")]`.
macro_rules! strict_label {
    ($($type:ty),+ $(,)?) => {
        $(
            $crate::strict_shape::derived_shape_impls!($type);

            impl<'de> ::serde::Deserialize<'de> for $type {
                fn deserialize<D: ::serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    $crate::strict_shape::deserialize_label(deserializer)
                }
            }
        )+
    };
}

pub(crate) use derived_shape_impls;
pub(crate) use strict_label;
pub(crate) use strict_object;

#[cfg(test)]
mod tests;
