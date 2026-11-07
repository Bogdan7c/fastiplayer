//! Адрес-заглушка номерка ресурса HLS (протокол 1.1, решение 228) — часть договора
//! Rust ↔ TypeScript.
//!
//! Фон экстеншена переписывает каждый адрес в плейлисте HLS (варианты, сегменты,
//! `EXT-X-MAP`, ключи) на адрес одного вида:
//!
//! ```text
//! https://resource.fastiplayer-bridge.invalid/<N>
//! ```
//!
//! где `<N>` — номерок [`ResourceHandle`] десятичным числом. Домен `.invalid`
//! зарезервирован (RFC 2606, раздел 2) и никуда не ведёт: даже если адрес случайно
//! уйдёт в обычную сеть, запрос никуда не попадёт.
//!
//! Разбор строгий: адрес — ровно эта строка, без вариантов. Ровно схема `https`
//! в нижнем регистре, ровно этот хост, без порта, имени пользователя, запроса
//! (`?`) и фрагмента (`#`), путь — `/` и номерок без ведущих нулей (кроме самого
//! `0`), без `%`-кодирования, знака и пробелов. Всё остальное транспорт плеера
//! отвергает у себя и в протокол не шлёт: так плеер не попросит у фона ничего,
//! кроме номерков, которые фон выдал сам.
//!
//! Тот же договор у TypeScript (`resource-address.ts`); общий список верных и
//! неверных адресов — `fixtures/resource-addresses.json`.

use crate::resource::ResourceHandle;

/// Хост адреса-заглушки.
pub const RESOURCE_ADDRESS_HOST: &str = "resource.fastiplayer-bridge.invalid";

/// Начало адреса-заглушки до номерка: схема, хост и `/`.
const RESOURCE_ADDRESS_PREFIX: &str = "https://resource.fastiplayer-bridge.invalid/";

/// Адрес-заглушка номерка: `https://resource.fastiplayer-bridge.invalid/<N>`.
#[must_use]
pub fn format_resource_address(handle: ResourceHandle) -> String {
    format!("{RESOURCE_ADDRESS_PREFIX}{}", handle.0)
}

/// Номерок из адреса-заглушки; `None` — адрес не того вида (правила — в
/// документации модуля).
#[must_use]
pub fn parse_resource_address(address: &str) -> Option<ResourceHandle> {
    let digits = address.strip_prefix(RESOURCE_ADDRESS_PREFIX)?;
    if !is_canonical_decimal(digits) {
        return None;
    }
    // Только цифры и без ведущих нулей: ошибка разбора здесь — лишь переполнение u32.
    digits.parse::<u32>().ok().map(ResourceHandle)
}

/// Непустая строка из одних ASCII-цифр без ведущих нулей (кроме самого «0»).
///
/// Проверка нужна до `str::parse`: тот принимает знак `+` и ведущие нули, а у
/// одного номерка должен быть ровно один адрес.
fn is_canonical_decimal(digits: &str) -> bool {
    let all_digits = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    let leading_zero = digits.len() > 1 && digits.starts_with('0');
    all_digits && !leading_zero
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trips_through_format_and_parse_for_edge_handles() {
        for number in [0, 1, 42, u32::MAX] {
            let handle = ResourceHandle(number);
            let address = format_resource_address(handle);
            assert_eq!(parse_resource_address(&address), Some(handle), "{address}");
        }
        assert_eq!(
            format_resource_address(ResourceHandle(7)),
            "https://resource.fastiplayer-bridge.invalid/7"
        );
    }

    #[test]
    fn prefix_names_exactly_the_reserved_host() {
        assert_eq!(
            RESOURCE_ADDRESS_PREFIX,
            format!("https://{RESOURCE_ADDRESS_HOST}/")
        );
    }

    #[test]
    fn anything_but_the_canonical_form_is_rejected() {
        for address in [
            "https://resource.fastiplayer-bridge.invalid/",
            "https://resource.fastiplayer-bridge.invalid/007",
            "https://resource.fastiplayer-bridge.invalid/+7",
            "https://resource.fastiplayer-bridge.invalid/4294967296",
            "https://resource.fastiplayer-bridge.invalid/7?x=1",
            "https://resource.fastiplayer-bridge.invalid:443/7",
            "http://resource.fastiplayer-bridge.invalid/7",
        ] {
            assert_eq!(parse_resource_address(address), None, "{address}");
        }
    }
}
