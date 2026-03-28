//! Parsed RESP3 response types.

use std::collections::HashMap;

/// A parsed RESP3 response value.
#[derive(Debug, Clone)]
pub enum Response {
    String(String),
    Error(String),
    Integer(i64),
    Null,
    Array(Vec<Response>),
    Map(Vec<(Response, Response)>),
}

impl Response {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Response::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Response::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Response::Error(_))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Response::Null)
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Response::String(_) => "String",
            Response::Error(_) => "Error",
            Response::Integer(_) => "Integer",
            Response::Null => "Null",
            Response::Array(_) => "Array",
            Response::Map(_) => "Map",
        }
    }

    pub fn to_display_string(&self) -> String {
        match self {
            Response::String(s) => s.clone(),
            Response::Error(e) => format!("(error) {e}"),
            Response::Integer(n) => n.to_string(),
            Response::Null => "(nil)".to_string(),
            Response::Array(_) => "(array)".to_string(),
            Response::Map(_) => "(map)".to_string(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Response::String(s) => serde_json::Value::String(s.clone()),
            Response::Error(e) => serde_json::json!({ "error": e }),
            Response::Integer(n) => serde_json::json!(n),
            Response::Null => serde_json::Value::Null,
            Response::Array(items) => {
                serde_json::Value::Array(items.iter().map(|r| r.to_json()).collect())
            }
            Response::Map(entries) => {
                let obj: serde_json::Map<String, serde_json::Value> = entries
                    .iter()
                    .map(|(k, v)| (k.to_display_string(), v.to_json()))
                    .collect();
                serde_json::Value::Object(obj)
            }
        }
    }

    pub fn to_raw(&self) -> String {
        let mut buf = String::new();
        write_raw(self, &mut buf);
        buf
    }

    pub fn print(&self, indent: usize) {
        let pad = "  ".repeat(indent);
        match self {
            Response::String(s) => println!("{pad}{s}"),
            Response::Error(e) => println!("{pad}(error) {e}"),
            Response::Integer(n) => println!("{pad}(integer) {n}"),
            Response::Null => println!("{pad}(nil)"),
            Response::Array(items) => {
                if items.is_empty() {
                    println!("{pad}(empty array)");
                } else {
                    for (i, item) in items.iter().enumerate() {
                        print!("{pad}{}. ", i + 1);
                        print_response_inline(item, indent + 1);
                    }
                }
            }
            Response::Map(entries) => {
                if entries.is_empty() {
                    println!("{pad}(empty map)");
                } else {
                    for (key, val) in entries {
                        let key_str = key.to_display_string();
                        match val {
                            Response::Map(_) | Response::Array(_) => {
                                println!("{pad}{key_str}:");
                                val.print(indent + 1);
                            }
                            _ => {
                                let val_str = response_to_inline_string(val);
                                println!("{pad}{key_str}: {val_str}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Look up a key in a map response.
    pub fn get_field(&self, key: &str) -> Option<&Response> {
        match self {
            Response::Map(entries) => entries
                .iter()
                .find(|(k, _)| k.to_display_string() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// Get a string field from a map response.
    pub fn get_string_field(&self, key: &str) -> Option<String> {
        self.get_field(key)
            .and_then(|v| v.as_str().map(String::from))
    }

    /// Get an integer field from a map response.
    pub fn get_int_field(&self, key: &str) -> Option<i64> {
        self.get_field(key).and_then(|v| match v {
            Response::Integer(n) => Some(*n),
            Response::String(s) => s.parse().ok(),
            _ => None,
        })
    }

    /// Convert a map response to a HashMap.
    pub fn to_hash_map(&self) -> Option<HashMap<String, serde_json::Value>> {
        match self {
            Response::Map(entries) => {
                let map: HashMap<String, serde_json::Value> = entries
                    .iter()
                    .map(|(k, v)| (k.to_display_string(), v.to_json()))
                    .collect();
                Some(map)
            }
            _ => None,
        }
    }
}

fn print_response_inline(resp: &Response, indent: usize) {
    match resp {
        Response::Map(_) | Response::Array(_) => {
            println!();
            resp.print(indent);
        }
        _ => {
            println!("{}", response_to_inline_string(resp));
        }
    }
}

fn response_to_inline_string(resp: &Response) -> String {
    match resp {
        Response::String(s) => s.clone(),
        Response::Error(e) => format!("(error) {e}"),
        Response::Integer(n) => format!("(integer) {n}"),
        Response::Null => "(nil)".to_string(),
        Response::Array(items) => format!("(array, {} items)", items.len()),
        Response::Map(entries) => format!("(map, {} entries)", entries.len()),
    }
}

fn write_raw(resp: &Response, buf: &mut String) {
    match resp {
        Response::String(s) => {
            buf.push_str(&format!("${}\r\n{s}\r\n", s.len()));
        }
        Response::Error(e) => {
            buf.push('-');
            buf.push_str(e);
            buf.push_str("\r\n");
        }
        Response::Integer(n) => {
            buf.push(':');
            buf.push_str(&n.to_string());
            buf.push_str("\r\n");
        }
        Response::Null => {
            buf.push_str("_\r\n");
        }
        Response::Array(items) => {
            buf.push_str(&format!("*{}\r\n", items.len()));
            for item in items {
                write_raw(item, buf);
            }
        }
        Response::Map(entries) => {
            buf.push_str(&format!("%{}\r\n", entries.len()));
            for (k, v) in entries {
                write_raw(k, buf);
                write_raw(v, buf);
            }
        }
    }
}
