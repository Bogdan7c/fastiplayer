#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::approx_constant)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::redundant_static_lifetimes)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::ptr_offset_with_cast)]
#![allow(clippy::useless_transmute)]
#![allow(unpredictable_function_pointer_comparisons)]
#![allow(unnecessary_transmutes)]
// Fastiplayer: bindgen переносит из glibc `string.h`/`stdlib.h` объявления memcpy/
// malloc/... с `u64` вместо `usize`; на x86-64 Linux это один и тот же ABI.
// Registry-копия глушила предупреждение через cap-lints, path-патч — нет.
#![allow(suspicious_runtime_symbol_definitions)]

extern crate libc;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[macro_use]
mod avutil;
pub use avutil::*;
