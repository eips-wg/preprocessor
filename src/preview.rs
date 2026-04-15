/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    fs::File,
    path::{Component, Path, PathBuf},
};

use log::info;
use snafu::{ResultExt, Whatever};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const PREVIEW_ADDR: &str = "127.0.0.1:1111";
const INDEX_HTML: &str = "index.html";

pub fn serve(output_path: &Path) -> Result<(), Whatever> {
    if !output_path.is_dir() {
        snafu::whatever!(
            "preview output directory `{}` is missing; run `build-eips build` for this profile first",
            output_path.to_string_lossy()
        );
    }

    let server = match Server::http(PREVIEW_ADDR) {
        Ok(server) => server,
        Err(error) => {
            snafu::whatever!("unable to bind preview server on {PREVIEW_ADDR}: {error}")
        }
    };

    info!(
        "serving static preview from `{}` at http://{PREVIEW_ADDR}/",
        output_path.to_string_lossy()
    );

    for request in server.incoming_requests() {
        handle_request(output_path, request)?;
    }

    Ok(())
}

fn handle_request(output_path: &Path, request: Request) -> Result<(), Whatever> {
    match *request.method() {
        Method::Get | Method::Head => {}
        _ => {
            request
                .respond(Response::empty(StatusCode(405)))
                .whatever_context("unable to send preview method error response")?;
            return Ok(());
        }
    }

    let Some(path) = resolve_request_path(output_path, request.url()) else {
        request
            .respond(Response::empty(StatusCode(400)))
            .whatever_context("unable to send preview bad request response")?;
        return Ok(());
    };

    if !path.is_file() {
        request
            .respond(Response::empty(StatusCode(404)))
            .whatever_context("unable to send preview not found response")?;
        return Ok(());
    }

    let file = File::open(&path).with_whatever_context(|e| {
        format!(
            "unable to open preview asset `{}`: {e}",
            path.to_string_lossy()
        )
    })?;

    let response = if let Some(value) = content_type(&path) {
        Response::from_file(file).with_header(content_type_header(value))
    } else {
        Response::from_file(file)
    };

    request
        .respond(response)
        .with_whatever_context(|e| format!("unable to send preview response: {e}"))?;

    Ok(())
}

fn resolve_request_path(output_path: &Path, url: &str) -> Option<PathBuf> {
    let raw_path = url.split('?').next().unwrap_or("/");
    let mut resolved = output_path.to_path_buf();
    let mut saw_normal_component = false;

    for component in Path::new(raw_path.trim_start_matches('/')).components() {
        match component {
            Component::CurDir | Component::RootDir => {}
            Component::Normal(component) => {
                saw_normal_component = true;
                resolved.push(component);
            }
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }

    if raw_path.ends_with('/') || !saw_normal_component {
        return Some(resolved.join(INDEX_HTML));
    }

    if resolved.is_dir() {
        return Some(resolved.join(INDEX_HTML));
    }

    if resolved.extension().is_none() {
        let candidate = resolved.join(INDEX_HTML);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    Some(resolved)
}

fn content_type(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => Some("text/css; charset=utf-8"),
        Some("gif") => Some("image/gif"),
        Some("htm" | "html") => Some("text/html; charset=utf-8"),
        Some("ico") => Some("image/x-icon"),
        Some("jpeg" | "jpg") => Some("image/jpeg"),
        Some("js") => Some("application/javascript; charset=utf-8"),
        Some("json") => Some("application/json; charset=utf-8"),
        Some("mjs") => Some("application/javascript; charset=utf-8"),
        Some("png") => Some("image/png"),
        Some("svg") => Some("image/svg+xml"),
        Some("txt") => Some("text/plain; charset=utf-8"),
        Some("webp") => Some("image/webp"),
        Some("xml") => Some("application/xml; charset=utf-8"),
        _ => None,
    }
}

fn content_type_header(value: &str) -> Header {
    Header::from_bytes(b"Content-Type", value.as_bytes())
        .expect("hard-coded content-type headers must be valid")
}
