// Конфиг стендового перехватчика. Rust (stand_extension.rs) подставляет два значения при
// подготовке каталога расширения; незаменённая подстановка "@...@" = ошибка подготовки.
// Адрес журнала берём отсюда, а не из страницы: у iframe с чужого origin в URL может не быть
// параметра report_origin, а главный origin стенда известен заранее.
globalThis.BENCH_CONFIG = {
  hostName: "@HOST_NAME@",
  reportOrigin: "@REPORT_ORIGIN@",
};
