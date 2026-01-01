/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use chrono::DateTime;

use citationberg::Style;
use eipw_preamble::Preamble;

use hayagriva::archive::ArchivedStyle;
use hayagriva::{BibliographyDriver, BibliographyRequest, CitationItem, CitationRequest};
use lazy_static::lazy_static;

use log::{debug, info, log_enabled, Level};
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

use pulldown_cmark_to_cmark::cmark;

use regex::Regex;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::read_to_string;
use std::io::Write;
use std::path::{Path, PathBuf};

use snafu::{whatever, OptionExt, ResultExt, Whatever};

use toml::Value;

use toml_datetime::Datetime;

use walkdir::WalkDir;

use iref::IriRefBuf;

use crate::progress::ProgressIteratorExt;

#[derive(Debug, Serialize, Deserialize)]
struct Author {
    name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    github: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

impl From<Author> for Value {
    fn from(value: Author) -> Self {
        // TODO: Hacky way to implement this conversion...
        toml::from_str(&toml::to_string(&value).unwrap()).unwrap()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FrontMatter {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<Datetime>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated: Option<Datetime>,

    #[serde(default, skip_serializing_if = "is_zero")]
    weight: usize,

    #[serde(default, skip_serializing_if = "is_false")]
    draft: bool,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    slug: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    path: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<PathBuf>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,

    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    in_search_index: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    template: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    taxonomies: HashMap<String, Vec<String>>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    extra: HashMap<String, Value>,
}

fn default_true() -> bool {
    true
}
fn is_true(x: &bool) -> bool {
    *x
}
fn is_false(x: &bool) -> bool {
    !*x
}
fn is_zero(x: &usize) -> bool {
    *x == 0
}

impl Default for FrontMatter {
    fn default() -> Self {
        Self {
            title: Default::default(),
            description: Default::default(),
            date: Default::default(),
            updated: Default::default(),
            weight: Default::default(),
            draft: Default::default(),
            slug: Default::default(),
            path: Default::default(),
            aliases: Default::default(),
            authors: Default::default(),
            in_search_index: true,
            template: Default::default(),
            taxonomies: Default::default(),
            extra: Default::default(),
        }
    }
}

fn last_modified(p: &Path) -> Result<Datetime, Whatever> {
    // TODO: Replace this with `git2`
    let mut command = std::process::Command::new("git");
    command
        .current_dir(p.parent().unwrap())
        .arg("log")
        .arg("-1")
        .arg("--pretty=format:%ct")
        .arg("--")
        .arg(p.file_name().unwrap());

    let output = command
        .output()
        .with_whatever_context(|e| format!("failed to execute {:?}: {e}", command))?;

    if !output.status.success() {
        let err_str = std::str::from_utf8(&output.stderr).unwrap_or("<non-utf-8>");
        whatever!("command {:?} failed: {err_str}", command);
    }

    let date_str = std::str::from_utf8(&output.stdout)
        .with_whatever_context(|e| format!("command {:?} output not UTF-8: {e}", command))?;

    let unix: i64 = date_str.parse().with_whatever_context(|e| {
        let err_str = std::str::from_utf8(&output.stderr).unwrap_or("<non-utf-8>");
        format!(
            "unable to parse timestamp `{date_str}` from {:?}: {e}\n{err_str}",
            command
        )
    })?;

    let date_time = DateTime::from_timestamp(unix, 0).unwrap();

    Ok(date_time.to_rfc3339().parse().unwrap())
}

fn write_file(path: &Path, front_matter: FrontMatter, body: &str) -> std::io::Result<()> {
    let mut output = std::fs::OpenOptions::new()
        .truncate(true)
        .write(true)
        .open(path)?;
    writeln!(output, "+++")?;
    writeln!(output, "{}", toml::to_string(&front_matter).unwrap())?;
    writeln!(output, "+++")?;
    writeln!(output, "{}", body)?;
    Ok(())
}

lazy_static! {
    // Matches GitHub usernames.
    static ref RE_GITHUB: Regex = Regex::new(r"^([^()<>,@]+) \(@([a-zA-Z\d-]+)\)$").unwrap();
    // Matches email addresses.
    static ref RE_EMAIL: Regex = Regex::new(r"^([^()<>,@]+) <([^@][^>]*@[^>]+\.[^>]+)>$").unwrap();
    // Matches a GitHub username plus email address.
    static ref RE_BOTH: Regex =
        Regex::new(r"^([^()<>,@]+) \(@([a-zA-Z\d-]+)\) <([^@][^>]*@[^>]+\.[^>]+)>$").unwrap();
    // Matches just a name.
    static ref RE_NAME: Regex = Regex::new(r"^([^()<>,@]+)$").unwrap();
}

fn extract_authors(value: &str) -> Result<Vec<Author>, Whatever> {
    let mut authors = Vec::new();
    let items = value.split(',').map(|x| x.trim());
    for item in items {
        if let Some(both) = RE_BOTH.captures(item) {
            authors.push(Author {
                name: both.get(1).unwrap().as_str().into(),
                github: Some(both.get(2).unwrap().as_str().into()),
                email: Some(both.get(3).unwrap().as_str().into()),
            });
        } else if let Some(email) = RE_EMAIL.captures(item) {
            authors.push(Author {
                name: email.get(1).unwrap().as_str().into(),
                github: None,
                email: Some(email.get(2).unwrap().as_str().into()),
            });
        } else if let Some(github) = RE_GITHUB.captures(item) {
            authors.push(Author {
                name: github.get(1).unwrap().as_str().into(),
                email: None,
                github: Some(github.get(2).unwrap().as_str().into()),
            });
        } else if let Some(name) = RE_NAME.captures(item) {
            authors.push(Author {
                name: name.get(1).unwrap().as_str().into(),
                email: None,
                github: None,
            });
        } else {
            whatever!("invalid author");
        }
    }
    Ok(authors)
}

pub fn preprocess(root_path: &Path) -> Result<(), Whatever> {
    let dir = std::fs::read_dir(root_path).with_whatever_context(|_| {
        format!("could not read directory `{}`", root_path.to_string_lossy())
    })?;
    let dirs: Vec<_> = dir.collect();

    info!("preprocessing markdown");

    for entry in dirs.into_iter().progress_ext("Markdown") {
        let entry = entry.with_whatever_context(|_| {
            format!(
                "could not read directory entry in `{}`",
                root_path.to_string_lossy()
            )
        })?;
        let entry_path = entry.path();

        let file_type = entry.file_type().with_whatever_context(|_| {
            format!(
                "could not get file type for `{}`",
                entry.path().to_string_lossy()
            )
        })?;

        if log_enabled!(Level::Debug) {
            let relative = match entry_path.strip_prefix(root_path) {
                Ok(r) => r,
                Err(_) => &entry_path,
            };
            match relative.with_extension("").to_string_lossy().parse::<u64>() {
                Ok(n) => debug!("preprocessing {}", n),
                Err(_) => debug!("preprocessing `{}`", relative.to_string_lossy()),
            }
        }

        if file_type.is_dir() {
            process_eip(root_path, &entry_path.join("index.md"))?;
            process_assets(root_path, &entry_path)?;
        } else if entry_path.extension().and_then(OsStr::to_str) == Some("md") {
            process_eip(root_path, &entry_path)?;
        }
    }

    Ok(())
}

fn path_to_at(root: &Path, parent: &Path, input: &str) -> Result<String, Whatever> {
    let croot = std::fs::canonicalize(root).with_whatever_context(|_| {
        format!("could not canonicalize `{}`", root.to_string_lossy())
    })?;

    let child = if input.starts_with("/") {
        let mut path = Path::new(input);
        path = path.strip_prefix("/").unwrap();
        root.join(path)
    } else {
        parent.join(Path::new(input))
    };

    let cchild = canonicalize_md(&child)?;
    let relative = cchild.strip_prefix(&croot).expect("child not in root");
    Ok(format!("@/{}", relative.to_str().unwrap()))
}

fn canonicalize_md(path: &Path) -> Result<PathBuf, Whatever> {
    let first_error = match std::fs::canonicalize(path) {
        Ok(canon) => return Ok(canon),
        Err(e) => e,
    };

    if path.extension() != Some(OsStr::new("md")) {
        panic!("canonicalizing non-md file: {}", path.to_string_lossy());
    }

    let alt_path = match path.file_name().and_then(OsStr::to_str) {
        Some("index.md") => {
            let mut new_path = path.to_owned();
            new_path.pop();
            new_path.set_extension("md");
            new_path
        }
        _ => {
            let mut new_path = path.with_extension("");
            new_path.push("index.md");
            new_path
        }
    };

    if let Ok(canon) = std::fs::canonicalize(&alt_path) {
        return Ok(canon);
    };

    Err(first_error).with_whatever_context(|_| {
        format!(
            "could not canonicalize `{}` or `{}`",
            path.to_string_lossy(),
            alt_path.to_string_lossy()
        )
    })
}

fn fix_links<'a, 'b>(
    root: &'a Path,
    parent: &'a Path,
    mut e: Event<'b>,
) -> Result<Event<'b>, Whatever> {
    match &mut e {
        Event::Start(Tag::Image { dest_url, .. }) | Event::Start(Tag::Link { dest_url, .. }) => {
            let mut iri_ref = IriRefBuf::new(dest_url.clone().into_string())
                .map_err(|e| e.to_string())
                .whatever_context("invalid URL in image/link")?;

            if iri_ref.authority().is_some() {
                // Is a protocol-relative or absolute URL.
                return Ok(e);
            }

            if !iri_ref.path().ends_with(".md") {
                // Only markdown files need the `@` syntax.
                return Ok(e);
            }

            let canonicalized = path_to_at(root, parent, iri_ref.path())?;
            let path = iref::iri::Path::new(&canonicalized).expect("path is valid IRI");
            iri_ref.set_path(path);

            *dest_url = CowStr::from(iri_ref.into_string());
            Ok(e)
        }
        _ => Ok(e),
    }
}

struct RenderCsl {
    contents: Option<String>,
}

impl RenderCsl {
    fn render_csl<'a>(&mut self, event: Event<'a>) -> Result<Option<Event<'a>>, Whatever> {
        let text = match (&mut self.contents, event) {
            (contents @ None, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref lang))))
                if lang.as_ref() == "csl-json" =>
            {
                *contents = Some(String::new());
                return Ok(None);
            }
            (Some(_), Event::End(TagEnd::CodeBlock)) => self.contents.take().unwrap(),
            (Some(contents), Event::Text(text)) => {
                contents.push_str(&text);
                return Ok(None);
            }
            (Some(_), event) => {
                panic!("unknown event inside csl-json block: {event:#?}");
            }
            (None, e) => return Ok(Some(e)),
        };

        let mut value: serde_json::Value =
            serde_json::from_str(&text).whatever_context("invalid JSON in citation")?;

        // TODO: Once typst/citationberg#17 is merged, we can remove this line.
        value
            .as_object_mut()
            .whatever_context("citation is not a JSON object")?
            .remove("custom");

        let item: citationberg::json::Item =
            serde_json::from_value(value).whatever_context("citation not valid")?;

        let locales = hayagriva::archive::locales();
        let style = match ArchivedStyle::AmericanPsychologicalAssociation.get() {
            Style::Independent(i) => i,
            _ => unreachable!(),
        };
        let mut driver = BibliographyDriver::new();

        let items = vec![CitationItem::with_entry(&item)];
        driver.citation(CitationRequest::from_items(items, &style, &locales));

        let result = driver.finish(BibliographyRequest {
            style: &style,
            locale: None,
            locale_files: &locales,
        });

        let bib = result.bibliography.unwrap();
        let mut text = String::new();
        for item in bib.items {
            item.content
                .write_buf(&mut text, hayagriva::BufWriteFormat::Html)
                .unwrap();
        }

        Ok(Some(Event::InlineHtml(text.into())))
    }
}

fn transform_markdown(root: &Path, path: &Path, body: &str) -> Result<String, Whatever> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let parent = path.parent().unwrap();
    let mut csl = RenderCsl { contents: None };

    let events = Parser::new_ext(body, opts)
        .map(|e| fix_links(root, parent, e))
        .filter_map(|r| match r {
            Ok(e) => csl.render_csl(e).transpose(),
            err => Some(err),
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter();

    let mut output = String::with_capacity(body.len() + (body.len() / 100));

    cmark(events, &mut output).whatever_context("cannot write markdown")?;

    Ok(output)
}

fn process_assets(root: &Path, path: &Path) -> Result<(), Whatever> {
    let number_txt = path
        .file_name()
        .with_whatever_context(|| format!("no file name for `{}`", path.to_string_lossy()))?
        .to_str()
        .with_whatever_context(|| format!("non-UTF-8 in `{}`", path.to_string_lossy()))?;

    let number: u32 = number_txt.parse().with_whatever_context(|_| {
        format!("can't parse number for `{}`", path.to_string_lossy())
    })?;

    let assets_dir = path.join("assets");

    let dir = WalkDir::new(&assets_dir)
        .follow_links(true)
        .into_iter()
        .filter(|e| match e {
            Ok(f) if !f.file_type().is_file() => false,
            Ok(f) => f.path().extension().and_then(OsStr::to_str) == Some("md"),
            Err(_) => true,
        });
    let dirs: Vec<_> = dir.collect();

    for entry in dirs.into_iter().progress_ext("Assets") {
        let entry = entry.with_whatever_context(|_| {
            format!("couldn't read entry in `{}`", assets_dir.to_string_lossy())
        })?;

        let path = entry.path();
        let contents = read_to_string(path).with_whatever_context(|_| {
            format!("could not read file `{}`", path.to_string_lossy())
        })?;

        let contents = transform_markdown(root, path, &contents).with_whatever_context(|_| {
            format!(
                "unable to transform markdown for `{}`",
                path.to_string_lossy()
            )
        })?;

        let relative_path = path.strip_prefix(&assets_dir).unwrap();
        let relative_path = relative_path.with_file_name(relative_path.file_stem().unwrap());

        let alias_bases = [
            PathBuf::from(format!("/assets/eip-{number}/")),
            PathBuf::from(format!("/assets/erc-{number}/")),
        ];

        let mut aliases = Vec::with_capacity(alias_bases.len());

        for alias_base in &alias_bases {
            aliases.push(alias_base.join(&relative_path));
        }

        if relative_path.ends_with("README") || relative_path.ends_with("index") {
            let index_path = relative_path.parent().unwrap();
            for alias_base in &alias_bases {
                aliases.push(alias_base.join(index_path));
            }
        }

        let front_matter = FrontMatter {
            path: format!("{number}/assets/{}", relative_path.to_str().unwrap()),
            aliases,
            ..Default::default()
        };

        write_file(path, front_matter, &contents).whatever_context("couldn't write file")?;
    }

    Ok(())
}

fn process_eip(root: &Path, path: &Path) -> Result<(), Whatever> {
    let path_lossy = path.to_string_lossy();
    let contents = read_to_string(path)
        .with_whatever_context(|_| format!("could not read file `{}`", path_lossy))?;

    let (preamble, body) = Preamble::split(&contents)
        .with_whatever_context(|_| format!("couldn't split preamble for `{}`", path_lossy))?;

    let body = transform_markdown(root, path, body)
        .with_whatever_context(|_| format!("unable to transform markdown for `{path_lossy}`"))?;

    let preamble = Preamble::parse(Some(&path_lossy), preamble)
        .ok()
        .with_whatever_context(|| format!("couldn't parse preamble in `{}`", path_lossy))?;

    let updated = match path.file_name() {
        Some(x) if x == "_index.md" => None,
        _ => Some(last_modified(path)?),
    };

    let mut front_matter = FrontMatter {
        updated,
        ..Default::default()
    };

    for field in preamble.fields() {
        let value = field.value().trim();
        match field.name() {
            "title" => front_matter.title = value.to_owned(),
            "description" => front_matter.description = value.to_owned(),
            "created" => {
                let parsed = value.parse().with_whatever_context(|_| {
                    format!("couldn't parse created in `{}`", path_lossy)
                })?;
                front_matter.date = Some(parsed);
            }
            "status" => {
                if value != "Final" && value != "Living" {
                    front_matter.draft = true;
                }
                front_matter.extra.insert("status".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("status".into(), vec![value.into()]);
            }
            "type" => {
                front_matter.extra.insert("type".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("type".into(), vec![value.into()]);
            }
            "category" => {
                front_matter.extra.insert("category".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("category".into(), vec![value.into()]);
            }
            "eip" | "number" => {
                let number = value
                    .parse::<u32>()
                    .whatever_context("couldn't parse eip/number")?;

                front_matter.template = Some("eip.html".into());
                front_matter.slug = number.to_string();
                front_matter.extra.insert("number".into(), number.into());

                let alias_path = PathBuf::from(&path);
                if let Some(file_stem) = alias_path.file_stem() {
                    let root = match file_stem.to_str() {
                        Some("index") => alias_path.parent().unwrap().file_name().unwrap(),
                        _ => file_stem,
                    };
                    front_matter.aliases.push(root.into());
                }

                front_matter
                    .aliases
                    .push(format!("ERCS/erc-{number}").into());
                front_matter
                    .aliases
                    .push(format!("EIPS/eip-{number}").into());
            }
            "author" => {
                let authors = extract_authors(value)?;
                front_matter.authors = authors.iter().map(|a| a.name.clone()).collect();
                front_matter
                    .extra
                    .insert("author_details".into(), Value::from(authors));
            }
            "requires" => {
                let items: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .map(str::parse)
                    .collect::<Result<Vec<u32>, _>>()
                    .whatever_context("could not parse requires")?
                    .into_iter()
                    .map(|eip| {
                        let path = format!("/{eip:0>5}.md");
                        path_to_at(root, root, &path)
                    })
                    .collect::<Result<_, _>>()?;
                front_matter
                    .extra
                    .insert("requires".into(), Value::from(items));
            }
            other => {
                let name = other.replace('-', "_");
                front_matter.extra.insert(name, value.into());
            }
        }
    }

    write_file(Path::new(&path), front_matter, &body).whatever_context("couldn't write file")?;

    Ok(())
}
