/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use eipw_lint::reporters;
use eipw_snippets::{annotate_snippets::Renderer, Level, Message, Snippet};

use std::fmt;

fn escape_property(text: &str) -> String {
    text.replace("%", "%25")
        .replace("\r", "%0D")
        .replace("\n", "%0A")
        .replace(":", "%3A")
        .replace(",", "%2C")
}

fn escape_data(text: &str) -> String {
    text.replace("%", "%25")
        .replace("\r", "%0D")
        .replace("\n", "%0A")
}

trait PropVec {
    fn property<'a, T: fmt::Display>(
        &'a mut self,
        name: &str,
        value: Option<T>,
    ) -> &'a mut Vec<String>;
}

impl PropVec for Vec<String> {
    fn property<'a, T: fmt::Display>(
        &'a mut self,
        name: &str,
        value: Option<T>,
    ) -> &'a mut Vec<String> {
        if let Some(value) = value {
            self.push(format!("{name}={}", escape_property(&format!("{value}"))));
        }
        self
    }
}

#[derive(Default, Debug)]
struct Annotation<'a> {
    title: Option<&'a str>,
    file: Option<&'a str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    start_column: Option<usize>,
    end_column: Option<usize>,
}

impl fmt::Display for Annotation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut props = Vec::with_capacity(6);
        props
            .property("title", self.title)
            .property("file", self.file)
            .property("line", self.start_line)
            .property("endLine", self.end_line)
            .property("col", self.start_column)
            .property("endCol", self.end_column);
        write!(f, "{}", props.join(","))
    }
}

impl<'a> Annotation<'a> {
    fn new(title: Option<&'a str>, value: &'a Snippet<'a>) -> Self {
        Self {
            title,
            start_line: Some(value.line_start),
            file: value.origin.as_deref(),
            ..Default::default()
        }
    }
}

#[derive(Debug)]
pub struct Reporter {
    pub root: String,
}

impl Reporter {
    fn print(msg: &Message<'_>, annotation: Annotation<'_>) {
        let command = match msg.level {
            Level::Help | Level::Note | Level::Info => "notice",
            Level::Warning => "warning",
            _ => "error",
        };

        let renderer = Renderer::plain();
        let message = format!("{}", renderer.render(msg.into()));

        println!("::{command} {annotation}::{}", escape_data(&message));
    }
}

impl reporters::Reporter for Reporter {
    fn report(&self, mut msg: Message<'_>) -> Result<(), reporters::Error> {
        if msg.snippets.is_empty() {
            Self::print(
                &msg,
                Annotation {
                    title: Some(&msg.title),
                    ..Default::default()
                },
            );
        } else {
            for snippet in &mut msg.snippets {
                let origin = match &snippet.origin {
                    None => continue,
                    Some(o) => o,
                };

                let stripped = match origin.strip_prefix(&self.root) {
                    Some(o) => o,
                    None => continue,
                };

                let stripped = match stripped.strip_prefix("/") {
                    Some(s) => s,
                    None => stripped,
                };

                snippet.origin = Some(stripped.to_string().into());
            }

            for snippet in &msg.snippets {
                Self::print(&msg, Annotation::new(Some(&msg.title), snippet));
            }
        }

        Ok(())
    }
}
