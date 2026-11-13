//! Пауза и play в окне плеера после передачи (`--pause-play-after <с>`, 16d1).
//!
//! Живая проверка эфира: Twitch на паузу выгружает эфир и на play перезапускает его новой шкалой
//! от нуля (09b2). После «один писатель шкалы и у эфира» (решение 322) это надо увидеть живьём:
//! окно на паузе, через [`PAUSE_HOLD`] — play, эфир снова идёт. Жмёт помощник через MPRIS окна, как
//! медиаклавиша человека.

use std::time::{Duration, Instant};

use browser_bridge_bench::player_control::PlayerControl;

/// Сколько окно стоит на паузе: Twitch успевает выгрузить буфер эфира, как у зрителя, отошедшего на минуту.
const PAUSE_HOLD: Duration = Duration::from_secs(10);

/// Где сейчас план паузы.
enum Step {
    /// Пауза ещё впереди.
    PauseAt(Instant),
    /// Окно на паузе, play — в этот момент.
    PlayAt(Instant),
    /// Сделано или не просили.
    Done,
}

/// План «пауза → play»; один раз за прогон.
pub(crate) struct PausePlay {
    step: Step,
}

impl PausePlay {
    /// План от момента передачи; `None` в `after` — не трогать.
    pub(crate) fn planned(handed_off_at: Instant, after: Option<Duration>) -> Self {
        let step = after.map_or(Step::Done, |delay| Step::PauseAt(handed_off_at + delay));
        Self { step }
    }

    /// Пора — нажать. Сбой печатается, прогон не рвётся: это помощник человека, итог виден в журнале плеера.
    pub(crate) fn poll(&mut self, control: &PlayerControl, now: Instant) {
        match self.step {
            Step::PauseAt(at) if now >= at => {
                match control.pause() {
                    Ok(()) => println!("pause-play: pause pressed in the player window"),
                    Err(error) => println!("pause-play: pause failed: {error}"),
                }
                self.step = Step::PlayAt(now + PAUSE_HOLD);
            }
            Step::PlayAt(at) if now >= at => {
                match control.play() {
                    Ok(()) => println!("pause-play: play pressed in the player window"),
                    Err(error) => println!("pause-play: play failed: {error}"),
                }
                self.step = Step::Done;
            }
            Step::PauseAt(_) | Step::PlayAt(_) | Step::Done => {}
        }
    }
}
