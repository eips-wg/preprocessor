/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use chrono::DateTime;

use eipw_preamble::Preamble;

use lazy_static::lazy_static;

use regex::Regex;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::env::args;
use std::ffi::OsStr;
use std::fs::read_to_string;
use std::io::Write;
use std::path::{Path, PathBuf};

use snafu::{whatever, OptionExt, ResultExt, Whatever};

use toml::Value;

use toml_datetime::Datetime;

use walkdir::WalkDir;

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
        .open(&path)?;
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

fn main() -> Result<(), Whatever> {
    let root = args().nth(1).unwrap();
    let dir = std::fs::read_dir(&root)
        .with_whatever_context(|_| format!("could not read directory `{root}`"))?;

    for entry in dir {
        let entry = entry
            .with_whatever_context(|_| format!("could not read directory entry in `{root}`"))?;

        let file_type = entry.file_type().with_whatever_context(|_| {
            format!(
                "could not get file type for `{}`",
                entry.path().to_string_lossy()
            )
        })?;

        let path = entry.path();
        if file_type.is_dir() {
            process_eip(&path.join("index.md"))?;
            process_assets(&path)?;
        } else if path.extension().and_then(OsStr::to_str) == Some("md") {
            process_eip(&path)?;
        }
    }

    Ok(())
}

fn process_assets(path: &Path) -> Result<(), Whatever> {
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

    for entry in dir {
        let entry = entry.with_whatever_context(|_| {
            format!("couldn't read entry in `{}`", assets_dir.to_string_lossy())
        })?;

        let path = entry.path();
        let contents = read_to_string(&path).with_whatever_context(|_| {
            format!("could not read file `{}`", path.to_string_lossy())
        })?;

        let relative_path = path.strip_prefix(&assets_dir).unwrap();
        let relative_path = relative_path.with_file_name(relative_path.file_stem().unwrap());

        let mut front_matter = FrontMatter::default();
        front_matter.path = format!("{number}/assets/{}", relative_path.to_str().unwrap());

        write_file(path, front_matter, &contents).whatever_context("couldn't write file")?;
    }

    Ok(())
}

fn process_eip(path: &Path) -> Result<(), Whatever> {
    let path_lossy = path.to_string_lossy();
    let contents = read_to_string(&path)
        .with_whatever_context(|_| format!("could not read file `{}`", path_lossy))?;

    let (preamble, body) = Preamble::split(&contents)
        .with_whatever_context(|_| format!("couldn't split preamble for `{}`", path_lossy))?;

    let preamble = Preamble::parse(Some(&path_lossy), preamble)
        .ok()
        .with_whatever_context(|| format!("couldn't parse preamble in `{}`", path_lossy))?;

    let mut front_matter = FrontMatter::default();

    front_matter.updated = Some(last_modified(Path::new(&path))?);

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
            }
            "author" => {
                let authors = extract_authors(value)?;
                front_matter.authors = authors.iter().map(|a| a.name.clone()).collect();
                front_matter
                    .extra
                    .insert("author_details".into(), Value::from(authors));
            }
            "requires" => {
                let items: Vec<u32> = value
                    .split(',')
                    .map(str::trim)
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .whatever_context("could not parse requires")?;
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

    write_file(Path::new(&path), front_matter, body).whatever_context("couldn't write file")?;

    Ok(())
}
