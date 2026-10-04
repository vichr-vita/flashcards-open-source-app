use serde_json::{Value, json};

const BUDGET: usize = 24_000;
const PARTIAL_ROWS: &str = "This answer is partial: data.rows carries only the leading rows of the result, because the whole result did not fit the size limit of a single tool result, and data.rowsTruncated is true because the rest were dropped here rather than by your query. data.rowCount counts the rows you received and data.totalRowCount how many rows the statement produced, so compare the two before you answer and tell the user the answer is partial whenever the rows you are missing could change it. data.limit is still the limit you asked for rather than the number of rows delivered, so continuing from data.offset + data.limit would skip the rows dropped here. Nothing was written, so when those rows matter, ask again for less at a time: select fewer or narrower columns, add WHERE filters, or aggregate instead of listing rows.";

fn length(value: &str) -> usize {
    value.encode_utf16().count()
}

fn prefix(value: &str, limit: usize) -> String {
    let mut remaining = limit;
    value
        .chars()
        .take_while(|character| {
            let fits = remaining >= character.len_utf16();
            remaining = remaining.saturating_sub(character.len_utf16());
            fits
        })
        .collect()
}

fn row_prefix(envelope: &Value) -> Option<String> {
    let data = envelope.get("data")?;
    if data.get("statementType").and_then(Value::as_str) != Some("select") {
        return None;
    }
    let rows = data.get("rows").and_then(Value::as_array)?;
    let mut low = 1_usize;
    let mut high = rows.len().saturating_sub(1);
    let mut fitting = None;
    while low <= high {
        let count = low.saturating_add(high).checked_div(2)?;
        let mut candidate = envelope.clone();
        let data = candidate.get_mut("data").and_then(Value::as_object_mut)?;
        data.insert(
            "rows".to_owned(),
            json!(rows.iter().take(count).collect::<Vec<_>>()),
        );
        data.insert("rowCount".to_owned(), json!(count));
        data.insert("rowsTruncated".to_owned(), json!(true));
        data.insert("hasMore".to_owned(), json!(true));
        candidate
            .as_object_mut()?
            .insert("instructions".to_owned(), json!(PARTIAL_ROWS));
        let serialized = candidate.to_string();
        if length(&serialized) <= BUDGET {
            fitting = Some(serialized);
            low = count.saturating_add(1);
        } else {
            high = count.saturating_sub(1);
        }
    }
    fitting
}

fn preview(envelope: &Value, field: &str) -> String {
    let value = envelope.get(field).unwrap_or(&Value::Null).to_string();
    let mut candidate = envelope.clone();
    let Some(map) = candidate.as_object_mut() else {
        return candidate.to_string();
    };
    map.remove(field);
    let key = format!("{field}Preview");
    map.insert("truncated".to_owned(), json!(true));
    let mut available = BUDGET;
    loop {
        let preview = prefix(&value, available);
        map.insert(key.clone(), json!(preview));
        map.insert(
            "omittedChars".to_owned(),
            json!(length(&value).saturating_sub(length(&preview))),
        );
        let result = Value::Object(map.clone()).to_string();
        let size = length(&result);
        if size <= BUDGET || available == 0 {
            return result;
        }
        available = available.saturating_sub(size.saturating_sub(BUDGET));
    }
}

/// Matches the chat's UTF-16 budget while preserving a leading SELECT row set whenever it fits.
pub(super) fn cap(envelope: &Value) -> String {
    let serialized = envelope.to_string();
    if length(&serialized) <= BUDGET {
        return serialized;
    }
    if let Some(capped) = row_prefix(envelope) {
        return capped;
    }
    let field = if envelope.get("ok").and_then(Value::as_bool) == Some(false) {
        "details"
    } else {
        "data"
    };
    let capped = preview(envelope, field);
    if length(&capped) <= BUDGET {
        return capped;
    }
    let mut shortened = envelope.clone();
    if let Some(sql) = envelope.get("sql").and_then(Value::as_str)
        && let Some(map) = shortened.as_object_mut()
    {
        map.insert("sql".to_owned(), json!(prefix(sql, 500)));
    }
    preview(&shortened, field)
}
