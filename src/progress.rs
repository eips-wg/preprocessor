/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::iter::FusedIterator;

use indicatif::{
    MultiProgress, ProgressBar, ProgressBarIter, ProgressFinish, ProgressIterator, ProgressStyle,
};
use indicatif_log_bridge::LogWrapper;
use log::Log;

lazy_static::lazy_static! {
    static ref PROGRESS: MultiProgress = MultiProgress::new();
    static ref STYLE: ProgressStyle = {
        ProgressStyle::with_template("{prefix:>12.cyan.bold} [{bar:57}] {pos}/{len}")
            .unwrap()
            .progress_chars("=> ")
    };
}

pub fn init<L: 'static + Log>(logger: L) {
    LogWrapper::new(PROGRESS.clone(), logger)
        .try_init()
        .unwrap();
}

pub fn clear() {
    PROGRESS.clear().ok();
}

pub trait ProgressIteratorExt: ProgressIterator + ExactSizeIterator {
    fn progress_ext(self, prefix: &'static str) -> ProgressBarExtIter<Self>;
}

impl<T> ProgressIteratorExt for T
where
    T: ProgressIterator + ExactSizeIterator,
{
    fn progress_ext(self, prefix: &'static str) -> ProgressBarExtIter<Self> {
        let len = self.len();
        if len == 0 {
            return ProgressBarExtIter::Without;
        }

        let bar = ProgressBar::new(len as _);
        PROGRESS.add(bar.clone());
        let it = self
            .progress_with(bar.clone())
            .with_style(STYLE.clone())
            .with_prefix(prefix)
            .with_finish(ProgressFinish::AndLeave);

        ProgressBarExtIter::With { bar: Some(bar), it }
    }
}

pub enum ProgressBarExtIter<T> {
    With {
        bar: Option<ProgressBar>,
        it: ProgressBarIter<T>,
    },
    Without,
}

impl<S, T: Iterator<Item = S>> Iterator for ProgressBarExtIter<T> {
    type Item = S;

    fn next(&mut self) -> Option<Self::Item> {
        let (it, bar) = match self {
            Self::Without => return None,
            Self::With { it, bar } => (it, bar),
        };

        let item = it.next();

        if item.is_none() {
            if let Some(bar) = bar.take() {
                PROGRESS.remove(&bar);
            }
        }

        item
    }
}

impl<T: ExactSizeIterator> ExactSizeIterator for ProgressBarExtIter<T> {
    fn len(&self) -> usize {
        match self {
            Self::With { it, .. } => it.len(),
            Self::Without => 0,
        }
    }
}

impl<T: DoubleEndedIterator> DoubleEndedIterator for ProgressBarExtIter<T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::With { it, .. } => it.next_back(),
            Self::Without => None,
        }
    }
}

impl<T: FusedIterator> FusedIterator for ProgressBarExtIter<T> {}

impl<T> Drop for ProgressBarExtIter<T> {
    fn drop(&mut self) {
        let bar = match self {
            Self::With { bar, .. } => bar,
            Self::Without => return,
        };
        if let Some(bar) = bar.take() {
            PROGRESS.remove(&bar);
        }
    }
}

pub struct Git(Option<ProgressBar>);

impl Git {
    pub fn new() -> Self {
        Self(Some(
            PROGRESS
                .add(ProgressBar::no_length())
                .with_style(STYLE.clone()),
        ))
    }

    pub fn remote_callbacks(&self) -> git2::RemoteCallbacks<'_> {
        let mut cb = git2::RemoteCallbacks::new();

        cb.transfer_progress(move |stats| {
            let local = match &self.0 {
                Some(b) => b,
                None => return true,
            };

            if stats.received_objects() == stats.total_objects() {
                local.set_prefix("Deltas");
                local.set_length(stats.total_deltas() as _);
                local.set_position(stats.indexed_deltas() as _);
            } else if stats.total_objects() > 0 {
                local.set_prefix("Objects");
                local.set_length(stats.total_objects() as _);
                local.set_position(stats.received_objects() as _);
            }
            true
        });

        cb
    }
}

impl Drop for Git {
    fn drop(&mut self) {
        if let Some(bar) = self.0.take() {
            PROGRESS.remove(&bar);
        }
    }
}
