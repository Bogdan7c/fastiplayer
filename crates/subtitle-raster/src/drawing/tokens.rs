// Порт из libass 0.17.5: ass_drawing.c (drawing_tokenize, get_point, add_3_points,
// add_many_points, add_root_node).
//
// Copyright (C) 2016 Vabishchevich Nikolay <vabnick@gmail.com>
// Copyright (C) 2017 Oleg Oshmyan <chortos@inbox.lv>
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Токенизатор команд рисования `m n l b s p c` (libass: `drawing_tokenize`).
//!
//! Токены — список опорных точек с типом. Тип говорит, как точку использовать при построении
//! контура ([`super::parse_drawing`]): переместить перо, провести линию, взять в группу
//! кубической кривой или b-сплайна. Список в libass двусвязный; здесь это `Vec`, а «соседние
//! токены» — соседние индексы.
//!
//! ## Правила совместимости с VSFilter (комментарий libass)
//! Рисунок, у которого первая известная команда не `m`, отвергался в guliverkli; xy-VSFilter
//! смягчил правило: до первого узла игнорируются все известные команды, кроме `n`. Если перед
//! `n` был `m` (пусть и с неверными числами), `n` становится корневым узлом, иначе весь
//! рисунок отвергается. Команда `p` игнорируется, пока узлов меньше трёх.
//!
//! ## Границы работы
//! Каждый символ текста порождает не больше одного токена на одну прочитанную точку (две
//! цифры и разделитель), а `c` добавляет три токена только после уже прочитанного сплайна,
//! поэтому число токенов линейно ограничено длиной текста. Длину текста рисунка ограничивает
//! `RasterInputLimits::max_chars_per_cue` (текст рисунка входит в текст реплики).

use super::number::parse_number;
use crate::fixed_point::double_to_d6;
use crate::outline::Point;

/// Как использовать точку при построении контура (libass: `ASS_TokenType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenKind {
    /// `m`: переместить перо; закрывает текущий контур.
    Move,
    /// `n`: переместить перо, не закрывая контур.
    MoveWithoutClosing,
    /// `l`: линия до точки.
    Line,
    /// `b`: первая из трёх точек кубической кривой Безье.
    CubicBezier,
    /// `s`: первая из трёх начальных точек b-сплайна.
    BSpline,
    /// `s`/`p`/`c`: продолжение b-сплайна одной точкой.
    ExtendSpline,
}

/// Опорная точка рисунка с типом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Token {
    /// Как использовать точку.
    pub(super) kind: TokenKind,
    /// Координаты в 26.6.
    pub(super) point: Point,
}

/// Разбивает текст рисунка на токены; пустой список — рисунок отвергнут или в нём нет узлов.
///
/// Текст обрывается на первом NUL (C-строка). Неизвестные символы пропускаются.
pub(super) fn tokenize(text: &[u8]) -> Vec<Token> {
    let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
    Tokenizer::new(text).run()
}

/// Итог команды: продолжать разбор или отвергнуть рисунок целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    KeepParsing,
    RejectDrawing,
}

/// Состояние разбора: позиция в тексте, накопленные токены и счётчики libass.
struct Tokenizer<'text> {
    text: &'text [u8],
    pos: usize,
    tokens: Vec<Token>,
    /// Сколько узлов считается набранными для команды `p` (libass: `points`).
    points: usize,
    /// Индекс токена перед началом последнего сплайна (libass: `spline_start`); по нему `c`
    /// замыкает сплайн повтором его первых точек.
    spline_start: Option<usize>,
    /// Встречалась ли команда `m` (даже с неверными числами).
    move_seen: bool,
}

impl<'text> Tokenizer<'text> {
    fn new(text: &'text [u8]) -> Self {
        Self {
            text,
            pos: 0,
            tokens: Vec::new(),
            points: 0,
            spline_start: None,
            move_seen: false,
        }
    }

    /// Главный цикл `drawing_tokenize`.
    fn run(mut self) -> Vec<Token> {
        while let Some(&command) = self.text.get(self.pos) {
            self.pos += 1;
            match command {
                b'm' => self.command_move(),
                b'n' => {
                    if self.command_move_without_closing() == Verdict::RejectDrawing {
                        return Vec::new();
                    }
                }
                b'l' if self.has_root() => {
                    self.points += self.add_many_points(TokenKind::Line, 1);
                }
                b'b' if self.has_root() => {
                    self.points += self.add_many_points(TokenKind::CubicBezier, 3);
                }
                b's' if self.has_root() => self.command_spline(),
                b'p' => self.command_extend_spline(),
                b'c' => self.command_close_spline(),
                // Неизвестный символ или команда без корневого узла: ищем следующую команду.
                _ => {}
            }
        }
        self.tokens
    }

    /// Есть ли корневой узел (до него `l`, `b`, `s` игнорируются).
    fn has_root(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// `m`: корневой узел (если его ещё нет) и все следующие точки как перемещения.
    fn command_move(&mut self) {
        self.move_seen = true;
        if !self.has_root() && !self.add_root(TokenKind::Move) {
            return;
        }
        self.points += self.add_many_points(TokenKind::Move, 1);
    }

    /// `n`: как `m`, но контур не закрывается; без предшествующего `m` рисунок отвергается.
    fn command_move_without_closing(&mut self) -> Verdict {
        if !self.has_root() {
            let Some(point) = self.read_point() else {
                return Verdict::KeepParsing;
            };
            if !self.move_seen {
                return Verdict::RejectDrawing;
            }
            self.push_root(point, TokenKind::MoveWithoutClosing);
        }
        self.points += self.add_many_points(TokenKind::MoveWithoutClosing, 1);
        Verdict::KeepParsing
    }

    /// `s`: три начальные точки сплайна, затем продолжение (как `p`, в C — `fallthrough`).
    fn command_spline(&mut self) {
        self.spline_start = self.tokens.len().checked_sub(1);
        if !self.add_three_points(TokenKind::BSpline) {
            self.spline_start = None;
            return;
        }
        self.points += 3;
        self.command_extend_spline();
    }

    /// `p`: продолжение сплайна точками; нужно не меньше трёх узлов.
    fn command_extend_spline(&mut self) {
        if self.points < 3 {
            return;
        }
        self.points += self.add_many_points(TokenKind::ExtendSpline, 1);
    }

    /// `c`: замыкает сплайн, добавляя в конец три точки, начиная с узла `spline_start`.
    fn command_close_spline(&mut self) {
        let Some(mut source) = self.spline_start.take() else {
            return;
        };
        for _ in 0..3 {
            let Some(point) = self.tokens.get(source).map(|token| token.point) else {
                return;
            };
            self.tokens.push(Token {
                kind: TokenKind::ExtendSpline,
                point,
            });
            source += 1;
        }
    }

    /// Читает пару чисел как точку 26.6 (libass: `get_point`). При неудаче на втором числе
    /// первое остаётся прочитанным (позиция не откатывается), как в libass.
    fn read_point(&mut self) -> Option<Point> {
        let (x, after_x) = parse_number(self.text, self.pos)?;
        self.pos = after_x;
        let (y, after_y) = parse_number(self.text, self.pos)?;
        self.pos = after_y;
        Some(Point::new(double_to_d6(x), double_to_d6(y)))
    }

    /// Читает точку и делает её корневым узлом; `false` — точки нет.
    fn add_root(&mut self, kind: TokenKind) -> bool {
        match self.read_point() {
            Some(point) => {
                self.push_root(point, kind);
                true
            }
            None => false,
        }
    }

    fn push_root(&mut self, point: Point, kind: TokenKind) {
        self.tokens.push(Token { kind, point });
        self.points = 1;
    }

    /// Ровно три точки подряд (libass: `add_3_points`); `false` — их меньше трёх, при этом
    /// прочитанное уже съедено.
    fn add_three_points(&mut self, kind: TokenKind) -> bool {
        let Some(first) = self.read_point() else {
            return false;
        };
        let Some(second) = self.read_point() else {
            return false;
        };
        let Some(third) = self.read_point() else {
            return false;
        };
        for point in [first, second, third] {
            self.tokens.push(Token { kind, point });
        }
        true
    }

    /// Читает точки, пока они читаются; каждые `batch_size` точек превращаются в токены,
    /// неполный хвост отбрасывается, но съедается (libass: `add_many_points`). Возвращает
    /// число добавленных токенов.
    fn add_many_points(&mut self, kind: TokenKind, batch_size: usize) -> usize {
        let mut batch: Vec<Point> = Vec::with_capacity(batch_size);
        let mut added = 0;
        while self.pos < self.text.len() {
            let Some(point) = self.read_point() else {
                break;
            };
            batch.push(point);
            if batch.len() < batch_size {
                continue;
            }
            added += batch.len();
            self.tokens
                .extend(batch.drain(..).map(|point| Token { kind, point }));
        }
        added
    }
}

#[cfg(test)]
mod tests;
