//! Turning a provider's response into the values a form question expects.
//!
//! Three small languages meet here, all of them operator-authored:
//!
//! - **Variables**: `[DATE]`, `[START]`, or any of the integration's own
//!   options, substituted into URLs and identifiers.
//! - **Paths**: a JSONPath string such as `$.steps.count`.
//! - **Aggregators**: an object such as `{"$sum": "$.items[*].value"}` or
//!   `{"$date": "$.day"}`, which reduce or reinterpret what a path selects.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use chrono_tz::Tz;
use serde_json::Value;
use serde_json_path::JsonPath;
use std::collections::HashMap;

/// The instants a fetch is evaluated against.
///
/// `start`/`end` bound a historical backfill; for a routine pull all three are
/// the same moment.
#[derive(Debug, Clone, Copy)]
pub struct Instants {
    pub now: DateTime<Tz>,
    pub start: DateTime<Tz>,
    pub end: DateTime<Tz>,
}

impl Instants {
    pub fn at(now: DateTime<Tz>) -> Self {
        Self {
            now,
            start: now,
            end: now,
        }
    }

    pub fn range(now: DateTime<Tz>, start: DateTime<Tz>, end: DateTime<Tz>) -> Self {
        Self { now, start, end }
    }
}

const DATE: &str = "%Y-%m-%d";

/// Resolves one of the built-in variable names.
fn builtin(name: &str, at: &Instants) -> Option<String> {
    let now = at.now;
    Some(match name {
        "DATE" => now.format(DATE).to_string(),
        "DATE_TIME" => now.to_rfc3339_opts(SecondsFormat::Secs, true),
        "DATE_TIME_MIDNIGHT" => midnight(now, 0)?.to_rfc3339_opts(SecondsFormat::Secs, true),
        "DATE_TIME_TOMORROW_MIDNIGHT" => {
            midnight(now, 1)?.to_rfc3339_opts(SecondsFormat::Secs, true)
        }
        "DATE_TOMORROW" => (now + Duration::days(1)).format(DATE).to_string(),
        "START" => at.start.format(DATE).to_string(),
        "END" => at.end.format(DATE).to_string(),
        _ => return None,
    })
}

/// Midnight `offset_days` from the given instant, in its own timezone.
fn midnight(at: DateTime<Tz>, offset_days: i64) -> Option<DateTime<Tz>> {
    let date = at.date_naive() + Duration::days(offset_days);
    // A DST transition can make midnight nonexistent. Taking the first valid
    // instant of the day is closer to the intent than failing the whole fetch.
    let tz = at.timezone();
    tz.from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()
        .or_else(|| {
            (0..4).find_map(|hour| {
                tz.from_local_datetime(&date.and_hms_opt(hour, 0, 0)?)
                    .earliest()
            })
        })
}

/// Substitutes `[NAME]` placeholders in a URL or identifier.
///
/// Integration options shadow the built-ins, so a provider can define an option
/// called `DATE` and mean its own thing.
pub fn replace_variables(input: &str, options: &HashMap<String, String>, at: &Instants) -> String {
    let mut result = input.to_owned();

    for (name, value) in options {
        result = result.replace(&format!("[{name}]"), &query_escape(value));
    }

    for name in [
        "DATE",
        "DATE_TIME",
        "DATE_TIME_MIDNIGHT",
        "DATE_TIME_TOMORROW_MIDNIGHT",
        "DATE_TOMORROW",
        "START",
        "END",
    ] {
        if options.contains_key(name) {
            continue;
        }

        let placeholder = format!("[{name}]");
        if !result.contains(&placeholder) {
            continue;
        }

        if let Some(value) = builtin(name, at) {
            result = result.replace(&placeholder, &query_escape(&value));
        }
    }

    result
}

/// Percent-encoding for a query-string value, matching Go's `url.QueryEscape`.
///
/// Notably a space becomes `+` rather than `%20`, and `:` and `/` are escaped.
/// Providers that sign or compare the query string care about the difference.
fn query_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                escaped.push(*byte as char);
            }
            b' ' => escaped.push('+'),
            other => escaped.push_str(&format!("%{other:02X}")),
        }
    }

    escaped
}

/// Resolves an entity's identifier for one record.
///
/// A leading `$` makes it a JSONPath into the record; anything else is a
/// literal, which is how a grouping identifier such as `[DATE]` works.
pub fn evaluate_identifier(
    identifier: &str,
    options: &HashMap<String, String>,
    data: &Value,
    at: &Instants,
) -> String {
    let resolved = replace_variables(identifier, options, at);

    if !resolved.starts_with('$') {
        return resolved;
    }

    match query_one(&resolved, data) {
        Some(value) => render(value),
        // Go stringifies a missing value as Go's nil rendering. Kept, because
        // it is the identifier a record with no id would already have been
        // stored under.
        None => "<nil>".to_owned(),
    }
}

/// Renders an extracted value the way Go's `%v` would.
fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "<nil>".to_owned(),
        Value::Number(number) => match number.as_f64() {
            // Go parses every JSON number as a float and prints it with the
            // shortest representation that round-trips, so 100.0 is "100".
            Some(float) if float.fract() == 0.0 && float.abs() < 1e15 => {
                format!("{}", float as i64)
            }
            _ => number.to_string(),
        },
        other => other.to_string(),
    }
}

/// Evaluates a JSONPath, returning the first node it selects.
fn query_one<'a>(path: &str, data: &'a Value) -> Option<&'a Value> {
    let parsed = match JsonPath::parse(path) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(%path, error = %err, "not a valid JSONPath");
            return None;
        }
    };

    parsed.query(data).first()
}

/// Extracts one field from a record, following either a path or an aggregator.
///
/// `None` means "nothing to record here". Callers skip the field rather than
/// abandoning the whole record: a provider omitting one optional value must not
/// cost the user everything else in it.
pub fn extract_field(path: &Value, data: &Value, at: &Instants) -> Option<Value> {
    match path {
        Value::String(text) => query_one(text, data).cloned(),
        Value::Object(map) => {
            // Only the first entry is honoured, as in Go.
            let (op, args) = map.iter().next()?;
            if op.len() < 2 {
                return None;
            }

            aggregate(&op[1..], args, data, at)
        }
        _ => None,
    }
}

/// Extracts the record's timestamp, in Unix milliseconds.
pub fn extract_timestamp(path: Option<&Value>, data: &Value, at: &Instants) -> Option<i64> {
    let extracted = extract_field(path?, data, at)?;

    match extracted {
        Value::Number(number) => number.as_f64().map(|value| value as i64),
        _ => None,
    }
}

fn aggregate(name: &str, args: &Value, data: &Value, at: &Instants) -> Option<Value> {
    match name {
        "sum" => Some(Value::from(numbers(args, data).sum::<f64>())),
        "mean" => {
            let values: Vec<f64> = numbers(args, data).collect();
            if values.is_empty() {
                // Go returns 0 for an empty list rather than NaN, which would
                // not survive JSON serialisation.
                return Some(Value::from(0));
            }
            Some(Value::from(
                values.iter().sum::<f64>() / values.len() as f64,
            ))
        }
        "div" => divide(args, data),
        "date" => parse_date(args, data, at, DateFormat::Date),
        "date_time" => parse_date(args, data, at, DateFormat::Rfc3339),
        "date_time_notz" => parse_date(args, data, at, DateFormat::LocalDateTime),
        "date_time_merge" => merge_date_and_time(args, data, at),
        "current_time" => Some(Value::from(at.now.timestamp_millis())),
        other => {
            // `len` is defined in Go but never registered, so a definition using
            // it already yields nothing. Kept that way deliberately.
            tracing::warn!(aggregator = %other, "unknown aggregator");
            None
        }
    }
}

/// The numbers a path selects, skipping anything that is not one.
fn numbers<'a>(args: &Value, data: &'a Value) -> impl Iterator<Item = f64> + 'a {
    let items = args
        .as_str()
        .and_then(|path| query_one(path, data))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    items.into_iter().filter_map(|item| item.as_f64())
}

fn divide(args: &Value, data: &Value) -> Option<Value> {
    let args = args.as_array()?;
    if args.len() != 2 {
        return None;
    }

    let path = args[0].as_str()?;
    let divisor = args[1].as_f64()?;
    if divisor == 0.0 {
        // Zero rather than an error or infinity: a misconfigured divisor should
        // not poison the record with a value JSON cannot represent.
        return Some(Value::from(0));
    }

    let value = query_one(path, data)?.as_f64()?;
    Some(Value::from(value / divisor))
}

enum DateFormat {
    /// A bare date, at midnight in the user's timezone.
    Date,
    /// A timestamp carrying its own offset.
    Rfc3339,
    /// A timestamp with no offset, read in the user's timezone.
    LocalDateTime,
}

fn parse_date(args: &Value, data: &Value, at: &Instants, format: DateFormat) -> Option<Value> {
    let path = args.as_str()?;
    let text = query_one(path, data)?.as_str()?;

    let millis = match format {
        DateFormat::Date => {
            let date = NaiveDate::parse_from_str(text, DATE).ok()?;
            in_zone(date.and_hms_opt(0, 0, 0)?, at)?
        }
        DateFormat::Rfc3339 => DateTime::parse_from_rfc3339(text).ok()?.timestamp_millis(),
        DateFormat::LocalDateTime => {
            let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.3f").ok()?;
            in_zone(naive, at)?
        }
    };

    Some(Value::from(millis))
}

/// Combines a date field and a time field the provider reports separately.
fn merge_date_and_time(args: &Value, data: &Value, at: &Instants) -> Option<Value> {
    let args = args.as_array()?;
    if args.len() != 2 {
        return None;
    }

    let date = query_one(args[0].as_str()?, data)?.as_str()?;
    let time = query_one(args[1].as_str()?, data)?.as_str()?;

    let naive = NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").ok()?;
    Some(Value::from(in_zone(naive, at)?))
}

/// Interprets a naive timestamp in the user's timezone.
fn in_zone(naive: NaiveDateTime, at: &Instants) -> Option<i64> {
    at.now
        .timezone()
        .from_local_datetime(&naive)
        .earliest()
        .map(|resolved| resolved.timestamp_millis())
}

/// Converts a definition value read from Mongo into plain JSON.
///
/// Paths and aggregator arguments are authored as BSON but evaluated against a
/// JSON response, so they have to meet in the same representation.
pub fn bson_to_json(value: &mongodb::bson::Bson) -> Value {
    value.clone().into_relaxed_extjson()
}

/// Renders the integration's options as the strings variables substitute in.
pub fn option_values(
    defined: &HashMap<String, crate::model::IntegrationOption>,
    chosen: &mongodb::bson::Document,
) -> HashMap<String, String> {
    let mut mapped = HashMap::new();

    for name in defined.keys() {
        let Some(value) = chosen.get(name) else {
            continue;
        };

        mapped.insert(name.clone(), bson_display(value));
    }

    mapped
}

/// Stringifies an option value the way Go's `%v` would.
fn bson_display(value: &mongodb::bson::Bson) -> String {
    use mongodb::bson::Bson;

    match value {
        Bson::String(text) => text.clone(),
        Bson::Int32(number) => number.to_string(),
        Bson::Int64(number) => number.to_string(),
        Bson::Double(number) => render(&Value::from(*number)),
        Bson::Boolean(flag) => flag.to_string(),
        Bson::Null => "<nil>".to_owned(),
        other => render(&bson_to_json(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at() -> Instants {
        let now = chrono_tz::Europe::Amsterdam
            .with_ymd_and_hms(2026, 7, 30, 14, 30, 0)
            .unwrap();
        Instants::at(now)
    }

    fn no_options() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn substitutes_the_date_variables() {
        let at = at();
        assert_eq!(
            replace_variables("[DATE]", &no_options(), &at),
            "2026-07-30"
        );
        assert_eq!(
            replace_variables("[DATE_TOMORROW]", &no_options(), &at),
            "2026-07-31"
        );
    }

    #[test]
    fn escapes_substituted_values() {
        // Go's QueryEscape, so a colon is escaped and a space becomes a plus.
        let at = at();
        assert_eq!(
            replace_variables("[DATE_TIME]", &no_options(), &at),
            "2026-07-30T14%3A30%3A00%2B02%3A00"
        );
    }

    #[test]
    fn options_shadow_the_builtins() {
        let mut options = HashMap::new();
        options.insert("DATE".to_owned(), "custom".to_owned());
        assert_eq!(replace_variables("[DATE]", &options, &at()), "custom");
    }

    #[test]
    fn leaves_unknown_placeholders_alone() {
        assert_eq!(
            replace_variables("[NOPE]/x", &no_options(), &at()),
            "[NOPE]/x"
        );
    }

    #[test]
    fn an_identifier_can_be_a_path_or_a_literal() {
        let data = json!({ "id": "abc" });
        assert_eq!(
            evaluate_identifier("$.id", &no_options(), &data, &at()),
            "abc"
        );
        assert_eq!(
            evaluate_identifier("[DATE]", &no_options(), &data, &at()),
            "2026-07-30"
        );
    }

    #[test]
    fn a_whole_number_identifier_has_no_decimal_point() {
        let data = json!({ "id": 100 });
        assert_eq!(
            evaluate_identifier("$.id", &no_options(), &data, &at()),
            "100"
        );
    }

    #[test]
    fn a_missing_identifier_is_stable_rather_than_empty() {
        let data = json!({});
        assert_eq!(
            evaluate_identifier("$.id", &no_options(), &data, &at()),
            "<nil>"
        );
    }

    #[test]
    fn extracts_a_plain_path() {
        let data = json!({ "count": 42 });
        assert_eq!(
            extract_field(&json!("$.count"), &data, &at()),
            Some(json!(42))
        );
    }

    #[test]
    fn a_missing_path_extracts_nothing() {
        assert_eq!(extract_field(&json!("$.nope"), &json!({}), &at()), None);
    }

    #[test]
    fn sums_and_averages() {
        let data = json!({ "values": [1, 2, 3, "skipped"] });
        assert_eq!(
            extract_field(&json!({ "$sum": "$.values" }), &data, &at()),
            Some(json!(6.0))
        );
        assert_eq!(
            extract_field(&json!({ "$mean": "$.values" }), &data, &at()),
            Some(json!(2.0))
        );
    }

    #[test]
    fn averaging_nothing_is_zero_rather_than_nan() {
        let data = json!({ "values": [] });
        assert_eq!(
            extract_field(&json!({ "$mean": "$.values" }), &data, &at()),
            Some(json!(0))
        );
    }

    #[test]
    fn divides_and_refuses_to_divide_by_zero() {
        let data = json!({ "ms": 90000 });
        assert_eq!(
            extract_field(&json!({ "$div": ["$.ms", 1000] }), &data, &at()),
            Some(json!(90.0))
        );
        assert_eq!(
            extract_field(&json!({ "$div": ["$.ms", 0] }), &data, &at()),
            Some(json!(0))
        );
    }

    #[test]
    fn reads_a_date_in_the_users_timezone() {
        let data = json!({ "day": "2026-07-30" });
        let extracted = extract_field(&json!({ "$date": "$.day" }), &data, &at()).unwrap();
        // Midnight Amsterdam on that day is 22:00 UTC the day before.
        let expected = chrono_tz::Europe::Amsterdam
            .with_ymd_and_hms(2026, 7, 30, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(extracted, json!(expected));
    }

    #[test]
    fn an_rfc3339_timestamp_keeps_its_own_offset() {
        let data = json!({ "at": "2026-07-30T12:00:00Z" });
        let extracted = extract_field(&json!({ "$date_time": "$.at" }), &data, &at()).unwrap();
        assert_eq!(extracted, json!(1785412800000_i64));
    }

    #[test]
    fn merges_a_separate_date_and_time() {
        let data = json!({ "day": "2026-07-30", "time": "14:30" });
        let extracted = extract_field(
            &json!({ "$date_time_merge": ["$.day", "$.time"] }),
            &data,
            &at(),
        )
        .unwrap();
        assert_eq!(extracted, json!(at().now.timestamp_millis()));
    }

    #[test]
    fn an_unknown_aggregator_extracts_nothing() {
        let data = json!({ "values": [1] });
        assert_eq!(
            extract_field(&json!({ "$nope": "$.values" }), &data, &at()),
            None
        );
    }

    #[test]
    fn extracts_a_timestamp_as_milliseconds() {
        let data = json!({ "ts": 1_700_000_000_000_i64 });
        assert_eq!(
            extract_timestamp(Some(&json!("$.ts")), &data, &at()),
            Some(1_700_000_000_000)
        );
    }

    #[test]
    fn a_non_numeric_timestamp_extracts_nothing() {
        let data = json!({ "ts": "yesterday" });
        assert_eq!(extract_timestamp(Some(&json!("$.ts")), &data, &at()), None);
    }
}
