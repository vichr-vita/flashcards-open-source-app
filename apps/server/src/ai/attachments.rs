use crate::error::ApiError;
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;

fn unsupported() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "CHAT_ATTACHMENT_UNSUPPORTED_TYPE",
        "This file type is not supported for AI chat. Remove the file or save it as PDF, TXT, CSV, JSON, XML, Markdown, HTML, Python, JavaScript, TypeScript, YAML, XLS/XLSX, DOCX, or an image, then try again.",
    )
}

fn integer(data: &[u8], offset: usize, little: bool) -> Option<usize> {
    let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    usize::try_from(if little {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    })
    .ok()
}

fn short(data: &[u8], offset: usize) -> Option<usize> {
    let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(usize::from(u16::from_le_bytes(bytes)))
}

fn png(data: &[u8]) -> bool {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let mut offset = 8_usize;
    let mut header = false;
    while let Some(length) = integer(data, offset, false) {
        let Some(end) = offset
            .checked_add(12)
            .and_then(|start| start.checked_add(length))
            .filter(|end| *end <= data.len())
        else {
            return false;
        };
        let Some(kind) = data.get(offset.saturating_add(4)..offset.saturating_add(8)) else {
            return false;
        };
        if !header && (kind != b"IHDR" || length != 13) {
            return false;
        }
        header = true;
        if kind == b"IEND" {
            return length == 0 && end == data.len();
        }
        offset = end;
    }
    false
}

fn webp(data: &[u8]) -> bool {
    if data.len() < 20 || !data.starts_with(b"RIFF") || data.get(8..12) != Some(b"WEBP".as_slice())
    {
        return false;
    }
    if integer(data, 4, true).and_then(|size| size.checked_add(8)) != Some(data.len()) {
        return false;
    }
    if !matches!(data.get(12..16), Some(b"VP8 " | b"VP8L" | b"VP8X")) {
        return false;
    }
    integer(data, 16, true)
        .and_then(|size| size.checked_add(20)?.checked_add(size % 2))
        .is_some_and(|end| end <= data.len())
}

fn open_xml(data: &[u8], required: &[u8]) -> bool {
    if !data.starts_with(b"PK\x03\x04")
        && !data.starts_with(b"PK\x05\x06")
        && !data.starts_with(b"PK\x07\x08")
    {
        return false;
    }
    if !data
        .get(data.len().saturating_sub(65_557)..)
        .is_some_and(|tail| tail.windows(4).any(|window| window == b"PK\x05\x06"))
    {
        return false;
    }
    let mut offset = 0_usize;
    let mut types = false;
    let mut document = false;
    while let Some(relative) = data
        .get(offset..)
        .and_then(|tail| tail.windows(4).position(|window| window == b"PK\x03\x04"))
    {
        let start = offset.saturating_add(relative);
        let Some(name_length) = short(data, start.saturating_add(26)) else {
            break;
        };
        let Some(extra) = short(data, start.saturating_add(28)) else {
            break;
        };
        let Some(size) = integer(data, start.saturating_add(18), true) else {
            break;
        };
        let Some(flags) = short(data, start.saturating_add(6)) else {
            break;
        };
        let name_start = start.saturating_add(30);
        let name_end = name_start.saturating_add(name_length);
        let Some(name) = data.get(name_start..name_end) else {
            break;
        };
        let next = name_end
            .saturating_add(extra)
            .saturating_add(if flags & 8 == 0 { size } else { 0 });
        if name_length == 0 || next > data.len() || next <= start {
            break;
        }
        types |= name == b"[Content_Types].xml";
        document |= name == required;
        offset = next;
    }
    types && document
}

fn extension_type(extension: &str) -> Option<&'static str> {
    match extension {
        "csv" => Some("text/csv"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "html" => Some("text/html"),
        "js" => Some("text/javascript"),
        "json" => Some("application/json"),
        "log" | "sql" | "txt" => Some("text/plain"),
        "md" => Some("text/markdown"),
        "pdf" => Some("application/pdf"),
        "py" => Some("text/x-python"),
        "ts" => Some("application/typescript"),
        "xls" => Some("application/vnd.ms-excel"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "xml" => Some("text/xml"),
        "yaml" | "yml" => Some("application/x-yaml"),
        _ => None,
    }
}

fn alias(media: &str) -> Option<&str> {
    match media {
        "application/csv" | "text/comma-separated-values" => Some("text/csv"),
        "application/javascript" | "application/x-javascript" => Some("text/javascript"),
        "application/x-sql" | "text/x-sql" => Some("text/plain"),
        "application/x-typescript" | "text/typescript" | "text/x-typescript" => {
            Some("application/typescript")
        }
        "text/x-markdown" => Some("text/markdown"),
        "text/x-yaml" | "text/yaml" => Some("application/x-yaml"),
        "application/xml" => Some("text/xml"),
        "application/json"
        | "application/typescript"
        | "application/x-yaml"
        | "text/csv"
        | "text/html"
        | "text/javascript"
        | "text/markdown"
        | "text/plain"
        | "text/x-python"
        | "text/xml"
        | "application/pdf"
        | "application/vnd.ms-excel"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some(media),
        _ => None,
    }
}

pub(super) fn validate(part: &mut Value) -> Result<(), ApiError> {
    let original = part
        .get("mediaType")
        .and_then(Value::as_str)
        .ok_or_else(unsupported)?
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let image = part.get("type").and_then(Value::as_str) == Some("image");
    let canonical = if image {
        match original.as_str() {
            "image/jpeg" | "image/jpg" | "image/pjpeg" => "image/jpeg",
            "image/gif" => "image/gif",
            "image/png" => "image/png",
            "image/webp" => "image/webp",
            _ => return Err(unsupported()),
        }
    } else {
        let file = part
            .get("fileName")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(unsupported)?;
        let basename = file.rsplit(['/', '\\']).next().unwrap_or_default();
        if let Some((_, extension)) = basename
            .rsplit_once('.')
            .filter(|(_, extension)| !extension.trim().is_empty())
        {
            extension_type(&extension.trim().to_ascii_lowercase()).ok_or_else(unsupported)?
        } else {
            alias(&original).ok_or_else(unsupported)?
        }
    }
    .to_owned();
    let encoded = part
        .get("base64Data")
        .and_then(Value::as_str)
        .ok_or_else(unsupported)?
        .trim();
    let data = STANDARD.decode(encoded).map_err(|_| unsupported())?;
    if data.is_empty() || STANDARD.encode(&data) != encoded {
        return Err(unsupported());
    }
    let valid = match canonical.as_str() {
        "image/png" => png(&data),
        "image/jpeg" => {
            data.len() >= 4 && data.starts_with(b"\xff\xd8\xff") && data.ends_with(b"\xff\xd9")
        }
        "image/gif" => {
            data.len() >= 14
                && (data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"))
                && data.ends_with(b";")
        }
        "image/webp" => webp(&data),
        "application/pdf" => {
            data.len() >= 20
                && data.starts_with(b"%PDF-")
                && data
                    .get(data.len().saturating_sub(2048)..)
                    .is_some_and(|tail| tail.windows(5).any(|window| window == b"%%EOF"))
        }
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            open_xml(&data, b"word/document.xml")
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            open_xml(&data, b"xl/workbook.xml")
        }
        "application/vnd.ms-excel" => {
            data.len() >= 512 && data.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1")
        }
        _ => {
            data.iter()
                .all(|byte| *byte >= 0x20 || matches!(byte, 9 | 10 | 12 | 13))
                || data.len() >= 4
                    && data.len().is_multiple_of(2)
                    && (data.starts_with(b"\xff\xfe") || data.starts_with(b"\xfe\xff"))
        }
    };
    if !valid {
        return Err(unsupported());
    }
    let encoded = encoded.to_owned();
    let map = part.as_object_mut().ok_or_else(unsupported)?;
    map.insert("mediaType".to_owned(), Value::String(canonical));
    map.insert("base64Data".to_owned(), Value::String(encoded));
    Ok(())
}
