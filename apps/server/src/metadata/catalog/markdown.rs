use super::error;
use crate::error::ApiError;
use axum::http::StatusCode;
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};
use regex::Regex;
use serde_json::Value;
use std::{collections::BTreeMap, sync::OnceLock};
use uuid::Uuid;

fn unsafe_pattern() -> Option<&'static Regex> {
    static PATTERN: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)(?:^|[^a-z0-9])(?:w-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}(?:\.[0-9]+)?|(?:sha256[._-])?[0-9a-f]{64}|media[/._\\-]blobs[/._\\-]sha256)(?:$|[^a-z0-9])")
        ).as_ref().ok()
}
fn media_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && Uuid::parse_str(key).is_err()
        && unsafe_pattern().is_some_and(|pattern| !pattern.is_match(key))
}
fn unsafe_text(value: &str) -> bool {
    let mut text = value.trim().to_lowercase();
    for _ in 0..=4 {
        if unsafe_pattern().is_none_or(|pattern| pattern.is_match(&text))
            || text
                .split(['/', '\\', '?', '#'])
                .any(|s| Uuid::parse_str(s).is_ok())
        {
            return true;
        }
        for part in text.split("fcasset:").skip(1) {
            let key = part
                .split(|c: char| c.is_whitespace() || "<>()[]".contains(c))
                .next()
                .unwrap_or_default();
            if !media_key(key) {
                return true;
            }
        }
        let decoded = percent_decode_str(&text).decode_utf8();
        let Ok(decoded) = decoded else {
            return text.contains("%2f") || text.contains("%5c");
        };
        if decoded == text {
            break;
        }
        text = decoded.into_owned();
    }
    false
}
fn unsafe_error() -> ApiError {
    error(
        StatusCode::CONFLICT,
        "CATALOG_PUBLIC_MEDIA_KEY_NOT_PUBLIC",
        "Published catalog package contains a non-public media reference.",
    )
}

pub(super) fn safe_value(value: &Value) -> Result<(), ApiError> {
    match value {
        Value::String(value) if unsafe_text(value) => return Err(unsafe_error()),
        Value::Array(values) => {
            for value in values {
                safe_value(value)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if !matches!(key.as_str(), "packageId" | "packageVersionId" | "authorId") {
                    safe_value(value)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn safe_markdown(markdown: &str) -> Result<(), ApiError> {
    let mut code = false;
    let mut excluded = Vec::new();
    for (event, span) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                code = true;
                excluded.push(span);
            }
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Code(_) => excluded.push(span),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. })
                if unsafe_text(&dest_url) =>
            {
                return Err(unsafe_error());
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text)
                if !code && unsafe_text(&text) =>
            {
                return Err(unsafe_error());
            }
            _ => {}
        }
    }
    // Public responses include raw Markdown, so inspect titles and unused definitions too.
    excluded.sort_by_key(|range| range.start);
    let mut offset = 0;
    for range in excluded {
        if range.start > offset
            && unsafe_text(
                markdown
                    .get(offset..range.start)
                    .ok_or_else(ApiError::internal)?,
            )
        {
            return Err(unsafe_error());
        }
        offset = offset.max(range.end);
    }
    if unsafe_text(markdown.get(offset..).ok_or_else(ApiError::internal)?) {
        return Err(unsafe_error());
    }
    Ok(())
}

fn destination_range(
    source: &str,
    destination: &str,
    definition: bool,
) -> Option<std::ops::Range<usize>> {
    let offsets: Vec<_> = if definition {
        vec![source.find("]:")?.checked_add(2)?]
    } else if source.starts_with('<') {
        vec![1]
    } else {
        source
            .match_indices("](")
            .filter_map(|(offset, _)| offset.checked_add(2))
            .collect()
    };
    offsets.into_iter().find_map(|start| {
        let range = raw_destination(source, start)?;
        let raw = source.get(range.clone())?;
        // Let the Markdown parser decode entity references and backslash escapes.
        let probe = format!("[destination](<{raw}>)");
        let decoded = Parser::new(&probe).find_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => Some(dest_url.into_string()),
            _ => None,
        })?;
        (decoded == destination).then_some(range)
    })
}

fn raw_destination(source: &str, start: usize) -> Option<std::ops::Range<usize>> {
    let suffix = source.get(start..)?;
    let padding = suffix.len().checked_sub(suffix.trim_start().len())?;
    let start = start
        .checked_add(padding)?
        .checked_add(usize::from(suffix.trim_start().starts_with('<')))?;
    let tail = source.get(start..)?;
    let length = tail
        .find(|c: char| c.is_whitespace() || matches!(c, ')' | '>'))
        .unwrap_or(tail.len());
    let end = start.checked_add(length)?;
    Some(start..end)
}

/// Rewrite active Markdown destinations, including reference definitions, without touching code or captions.
pub(super) fn rewrite(markdown: &str, media: &BTreeMap<String, Uuid>) -> Result<String, ApiError> {
    let parser = Parser::new(markdown);
    let definitions: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, d)| (d.dest.to_string(), d.span.clone()))
        .collect();
    let mut edits = BTreeMap::new();
    for (destination, span, definition) in parser
        .into_offset_iter()
        .filter_map(|(event, span)| match event {
            Event::Start(
                Tag::Link {
                    dest_url,
                    link_type: LinkType::Inline | LinkType::Autolink,
                    ..
                }
                | Tag::Image {
                    dest_url,
                    link_type: LinkType::Inline,
                    ..
                },
            ) => Some((dest_url.to_string(), span, false)),
            _ => None,
        })
        .chain(definitions.into_iter().map(|(url, span)| (url, span, true)))
    {
        let Some(key) = destination.strip_prefix("fcasset:") else {
            continue;
        };
        let id = media.get(key).ok_or_else(|| {
            error(
                StatusCode::CONFLICT,
                "CATALOG_PACKAGE_INSTALL_MEDIA_ASSET_NOT_FOUND",
                "Catalog package card references missing package media asset.",
            )
        })?;
        let source = markdown.get(span.clone()).ok_or_else(ApiError::internal)?;
        if let Some(local) = destination_range(source, &destination, definition) {
            let start = span
                .start
                .checked_add(local.start)
                .ok_or_else(ApiError::internal)?;
            let end = span
                .start
                .checked_add(local.end)
                .ok_or_else(ApiError::internal)?;
            edits.insert(start, (end, format!("fcasset:{id}")));
        } else {
            return Err(error(
                StatusCode::CONFLICT,
                "CATALOG_PACKAGE_INSTALL_MEDIA_REWRITE_FAILED",
                "Catalog package Markdown destination cannot be rewritten safely.",
            ));
        }
    }
    let mut result = markdown.to_owned();
    for (start, (end, replacement)) in edits.into_iter().rev() {
        result.replace_range(start..end, &replacement);
    }
    Ok(result)
}
