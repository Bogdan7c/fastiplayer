//! Compact checked normalization Smooth Streaming chunk timeline.

use crate::error::{SmoothDeclaredCountKind, SmoothManifestError, SmoothTimelineError};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};
use crate::model::SmoothManifestVersion;
use crate::time::{SmoothTime, SmoothTimescale};
use crate::timeline_input::{
    SmoothChunkDuration, SmoothChunkEntry, SmoothChunkRepeat, SmoothChunkStart,
    SmoothDeclaredFragmentCount,
};

/// Один compact arithmetic run независимо от fragment count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmoothChunkRun {
    first_fragment_index: usize,
    first_start_ticks: u64,
    duration_ticks: u64,
    fragment_count: usize,
}

impl SmoothChunkRun {
    #[must_use]
    pub const fn first_fragment_index(&self) -> usize {
        self.first_fragment_index
    }

    #[must_use]
    pub const fn first_start_ticks(&self) -> u64 {
        self.first_start_ticks
    }

    #[must_use]
    pub const fn duration_ticks(&self) -> u64 {
        self.duration_ticks
    }

    #[must_use]
    pub const fn fragment_count(&self) -> usize {
        self.fragment_count
    }

    fn end_ticks(self) -> u64 {
        self.duration_ticks
            .checked_mul(
                u64::try_from(self.fragment_count)
                    .expect("validated usize fragment count помещается в u64"),
            )
            .and_then(|span| self.first_start_ticks.checked_add(span))
            .expect("run arithmetic проверена при normalization")
    }
}

/// Один lazily materialized fragment compact timeline-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmoothChunkFragment {
    index: usize,
    start: SmoothTime,
    duration_ticks: u64,
}

impl SmoothChunkFragment {
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    #[must_use]
    pub const fn start(self) -> SmoothTime {
        self.start
    }

    #[must_use]
    pub const fn duration_ticks(self) -> u64 {
        self.duration_ticks
    }
}

/// Immutable compact timeline хранит O(raw entries), а не O(fragments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothChunkTimeline {
    timescale: SmoothTimescale,
    runs: Box<[SmoothChunkRun]>,
    fragment_count: usize,
}

impl SmoothChunkTimeline {
    #[must_use]
    pub const fn timescale(&self) -> SmoothTimescale {
        self.timescale
    }

    #[must_use]
    pub const fn fragment_count(&self) -> usize {
        self.fragment_count
    }

    #[must_use]
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    #[must_use]
    pub fn runs(&self) -> &[SmoothChunkRun] {
        &self.runs
    }

    pub fn fragment_at(&self, index: usize) -> Result<SmoothChunkFragment, SmoothManifestError> {
        if index >= self.fragment_count {
            return Err(invalid_timeline(
                SmoothTimelineError::FragmentIndexOutOfRange,
            ));
        }
        let run = self
            .runs
            .iter()
            .rev()
            .find(|run| run.first_fragment_index <= index)
            .expect("validated nonempty timeline содержит run для каждого index");
        let offset = index - run.first_fragment_index;
        let offset_ticks = run
            .duration_ticks
            .checked_mul(
                u64::try_from(offset)
                    .map_err(|_| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?,
            )
            .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
        let start_ticks = run
            .first_start_ticks
            .checked_add(offset_ticks)
            .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
        Ok(SmoothChunkFragment {
            index,
            start: SmoothTime::new(start_ticks, self.timescale),
            duration_ticks: run.duration_ticks,
        })
    }

    #[must_use]
    pub fn iter_fragments(&self) -> SmoothChunkFragmentIter<'_> {
        SmoothChunkFragmentIter {
            timeline: self,
            run_index: 0,
            offset_in_run: 0,
            next_index: 0,
        }
    }

    #[must_use]
    pub fn first_start(&self) -> SmoothTime {
        SmoothTime::new(self.runs[0].first_start_ticks, self.timescale)
    }

    #[must_use]
    pub fn last_end(&self) -> SmoothTime {
        SmoothTime::new(
            self.runs
                .last()
                .expect("validated timeline непуст")
                .end_ticks(),
            self.timescale,
        )
    }
}

/// Iterator материализует не более одного fragment value за шаг.
pub struct SmoothChunkFragmentIter<'timeline> {
    timeline: &'timeline SmoothChunkTimeline,
    run_index: usize,
    offset_in_run: usize,
    next_index: usize,
}

impl Iterator for SmoothChunkFragmentIter<'_> {
    type Item = SmoothChunkFragment;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.timeline.fragment_count {
            return None;
        }
        let run = self
            .timeline
            .runs
            .get(self.run_index)
            .expect("validated iterator run всегда существует");
        let offset_ticks = run
            .duration_ticks
            .checked_mul(
                u64::try_from(self.offset_in_run)
                    .expect("validated fragment offset помещается в u64"),
            )
            .expect("validated run offset arithmetic не переполняется");
        let start_ticks = run
            .first_start_ticks
            .checked_add(offset_ticks)
            .expect("validated fragment start arithmetic не переполняется");
        let fragment = SmoothChunkFragment {
            index: self.next_index,
            start: SmoothTime::new(start_ticks, self.timeline.timescale),
            duration_ticks: run.duration_ticks,
        };
        self.next_index += 1;
        self.offset_in_run += 1;
        if self.offset_in_run == run.fragment_count {
            self.run_index += 1;
            self.offset_in_run = 0;
        }
        Some(fragment)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.timeline.fragment_count - self.next_index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for SmoothChunkFragmentIter<'_> {}

/// Manifest-scoped accumulator применяет total limits транзакционно.
pub(crate) struct SmoothManifestTimelineBudget<'limits> {
    limits: &'limits SmoothManifestLimits,
    accepted_timeline_entries: usize,
    accepted_fragments: usize,
}

impl<'limits> SmoothManifestTimelineBudget<'limits> {
    #[must_use]
    pub(crate) const fn new(limits: &'limits SmoothManifestLimits) -> Self {
        Self {
            limits,
            accepted_timeline_entries: 0,
            accepted_fragments: 0,
        }
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) const fn accepted_timeline_entries(&self) -> usize {
        self.accepted_timeline_entries
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) const fn accepted_fragments(&self) -> usize {
        self.accepted_fragments
    }

    #[cfg(test)]
    pub(crate) fn build_stream_timeline(
        &mut self,
        version: SmoothManifestVersion,
        timescale: SmoothTimescale,
        entries: &[SmoothChunkEntry],
        declared_count: SmoothDeclaredFragmentCount,
    ) -> Result<SmoothChunkTimeline, SmoothManifestError> {
        self.build_stream_timeline_cancellable(
            version,
            timescale,
            entries,
            declared_count,
            &mut || false,
        )
    }

    pub(crate) fn build_stream_timeline_cancellable(
        &mut self,
        version: SmoothManifestVersion,
        timescale: SmoothTimescale,
        entries: &[SmoothChunkEntry],
        declared_count: SmoothDeclaredFragmentCount,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<SmoothChunkTimeline, SmoothManifestError> {
        check_cancelled(is_cancelled)?;
        if entries.is_empty() {
            return Err(invalid_timeline(SmoothTimelineError::Empty));
        }
        enforce_limit(
            entries.len(),
            self.limits.maximum_timeline_entries_per_stream(),
            SmoothManifestLimitKind::TimelineEntriesPerStream,
        )?;
        let candidate_total_entries = self
            .accepted_timeline_entries
            .checked_add(entries.len())
            .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
        enforce_limit(
            candidate_total_entries,
            self.limits.maximum_total_timeline_entries(),
            SmoothManifestLimitKind::TotalTimelineEntries,
        )?;

        let (runs, fragment_count) = normalize_runs(
            version,
            entries,
            self.limits.maximum_fragments_per_stream(),
            is_cancelled,
        )?;
        let candidate_total_fragments = self
            .accepted_fragments
            .checked_add(fragment_count)
            .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
        enforce_limit(
            candidate_total_fragments,
            self.limits.maximum_total_fragments(),
            SmoothManifestLimitKind::TotalFragments,
        )?;
        validate_declared_count(declared_count, fragment_count)?;

        self.accepted_timeline_entries = candidate_total_entries;
        self.accepted_fragments = candidate_total_fragments;
        Ok(SmoothChunkTimeline {
            timescale,
            runs: runs.into_boxed_slice(),
            fragment_count,
        })
    }
}

fn normalize_runs(
    version: SmoothManifestVersion,
    entries: &[SmoothChunkEntry],
    maximum_fragments_per_stream: usize,
    is_cancelled: &mut dyn FnMut() -> bool,
) -> Result<(Vec<SmoothChunkRun>, usize), SmoothManifestError> {
    let mut runs = Vec::with_capacity(entries.len());
    let mut fragment_count = 0usize;
    let mut previous_end = None;

    for (entry_index, entry) in entries.iter().copied().enumerate() {
        check_cancelled(is_cancelled)?;
        let run_count = normalized_repeat(version, entry.repeat)?;
        let start_ticks = match entry.start {
            SmoothChunkStart::Explicit(start_ticks) => start_ticks,
            SmoothChunkStart::Inferred => previous_end.unwrap_or(0),
        };
        if let Some(expected_start) = previous_end {
            validate_contiguous_start(start_ticks, expected_start, &runs)?;
        }
        let duration_ticks = normalized_duration(entries, entry_index, start_ticks, run_count)?;
        let candidate_fragment_count = fragment_count
            .checked_add(run_count)
            .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
        enforce_limit(
            candidate_fragment_count,
            maximum_fragments_per_stream,
            SmoothManifestLimitKind::FragmentsPerStream,
        )?;
        let run = SmoothChunkRun {
            first_fragment_index: fragment_count,
            first_start_ticks: start_ticks,
            duration_ticks,
            fragment_count: run_count,
        };
        previous_end = Some(checked_run_end(run)?);
        runs.push(run);
        fragment_count = candidate_fragment_count;
    }
    Ok((runs, fragment_count))
}

fn normalized_repeat(
    version: SmoothManifestVersion,
    repeat: SmoothChunkRepeat,
) -> Result<usize, SmoothManifestError> {
    match repeat {
        SmoothChunkRepeat::ImplicitSingle => Ok(1),
        SmoothChunkRepeat::Declared(0) => Err(invalid_timeline(SmoothTimelineError::ZeroRepeat)),
        SmoothChunkRepeat::Declared(_) if version != SmoothManifestVersion::V2_2 => Err(
            invalid_timeline(SmoothTimelineError::RepeatRequiresVersion22),
        ),
        SmoothChunkRepeat::Declared(value) => usize::try_from(value)
            .map_err(|_| invalid_timeline(SmoothTimelineError::ArithmeticOverflow)),
    }
}

fn normalized_duration(
    entries: &[SmoothChunkEntry],
    entry_index: usize,
    start_ticks: u64,
    fragment_count: usize,
) -> Result<u64, SmoothManifestError> {
    match entries[entry_index].duration {
        SmoothChunkDuration::Explicit(0) => {
            Err(invalid_timeline(SmoothTimelineError::ZeroDuration))
        }
        SmoothChunkDuration::Explicit(duration_ticks) => Ok(duration_ticks),
        SmoothChunkDuration::InferFromNextExplicitStart => {
            let next_entry = entries.get(entry_index + 1).ok_or_else(|| {
                invalid_timeline(SmoothTimelineError::MissingAdjacentExplicitStart)
            })?;
            let SmoothChunkStart::Explicit(next_start) = next_entry.start else {
                return Err(invalid_timeline(
                    SmoothTimelineError::MissingAdjacentExplicitStart,
                ));
            };
            let span = next_start
                .checked_sub(start_ticks)
                .ok_or_else(|| invalid_timeline(SmoothTimelineError::BackwardStart))?;
            let divisor = u64::try_from(fragment_count)
                .map_err(|_| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
            if span % divisor != 0 {
                return Err(invalid_timeline(
                    SmoothTimelineError::NonDivisibleInferredDuration,
                ));
            }
            let duration_ticks = span / divisor;
            if duration_ticks == 0 {
                return Err(invalid_timeline(SmoothTimelineError::ZeroDuration));
            }
            Ok(duration_ticks)
        }
    }
}

fn validate_contiguous_start(
    start_ticks: u64,
    expected_start: u64,
    runs: &[SmoothChunkRun],
) -> Result<(), SmoothManifestError> {
    if start_ticks == expected_start {
        return Ok(());
    }
    if start_ticks > expected_start {
        return Err(invalid_timeline(SmoothTimelineError::Discontinuity));
    }
    let previous_start = runs
        .last()
        .expect("previous end существует только после первого run")
        .first_start_ticks;
    if start_ticks < previous_start {
        Err(invalid_timeline(SmoothTimelineError::BackwardStart))
    } else {
        Err(invalid_timeline(SmoothTimelineError::Overlap))
    }
}

fn checked_run_end(run: SmoothChunkRun) -> Result<u64, SmoothManifestError> {
    let fragment_count = u64::try_from(run.fragment_count)
        .map_err(|_| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
    let span = run
        .duration_ticks
        .checked_mul(fragment_count)
        .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
    run.first_start_ticks
        .checked_add(span)
        .ok_or_else(|| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))
}

fn validate_declared_count(
    declared_count: SmoothDeclaredFragmentCount,
    actual: usize,
) -> Result<(), SmoothManifestError> {
    match declared_count {
        SmoothDeclaredFragmentCount::Exact(declared) => {
            let actual = u64::try_from(actual)
                .map_err(|_| invalid_timeline(SmoothTimelineError::ArithmeticOverflow))?;
            if declared != actual {
                return Err(SmoothManifestError::DeclaredCountMismatch {
                    kind: SmoothDeclaredCountKind::FragmentCount,
                    declared,
                    actual,
                });
            }
            Ok(())
        }
        #[cfg(test)]
        SmoothDeclaredFragmentCount::Unspecified => Ok(()),
    }
}

fn enforce_limit(
    observed: usize,
    maximum: usize,
    limit: SmoothManifestLimitKind,
) -> Result<(), SmoothManifestError> {
    if observed > maximum {
        return Err(SmoothManifestError::LimitExceeded { limit, maximum });
    }
    Ok(())
}

fn invalid_timeline(reason: SmoothTimelineError) -> SmoothManifestError {
    SmoothManifestError::InvalidTimeline { reason }
}

fn check_cancelled(is_cancelled: &mut dyn FnMut() -> bool) -> Result<(), SmoothManifestError> {
    if is_cancelled() {
        Err(SmoothManifestError::Cancelled)
    } else {
        Ok(())
    }
}
