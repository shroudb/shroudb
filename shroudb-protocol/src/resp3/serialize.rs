use crate::response::{CommandResponse, ResponseMap, ResponseValue};

use super::Resp3Frame;

/// Convert a `CommandResponse` into a RESP3 frame for wire serialization.
pub fn response_to_frame(response: &CommandResponse) -> Resp3Frame {
    match response {
        CommandResponse::Success(map) => response_map_to_frame(map),
        CommandResponse::Error(err) => Resp3Frame::SimpleError(err.to_string()),
        CommandResponse::Array(items) => {
            Resp3Frame::Array(items.iter().map(response_to_frame).collect())
        }
        CommandResponse::CachedFrame(frame) => frame.clone(),
    }
}

fn response_map_to_frame(map: &ResponseMap) -> Resp3Frame {
    Resp3Frame::Map(
        map.fields
            .iter()
            .map(|(k, v)| {
                (
                    Resp3Frame::BulkString(k.as_bytes().to_vec()),
                    response_value_to_frame(v),
                )
            })
            .collect(),
    )
}

fn response_value_to_frame(value: &ResponseValue) -> Resp3Frame {
    match value {
        ResponseValue::String(s) => Resp3Frame::BulkString(s.as_bytes().to_vec()),
        ResponseValue::Integer(n) => Resp3Frame::Integer(*n),
        ResponseValue::Float(f) => Resp3Frame::BulkString(f.to_string().into_bytes()),
        ResponseValue::Boolean(b) => Resp3Frame::BulkString(if *b {
            b"true".to_vec()
        } else {
            b"false".to_vec()
        }),
        ResponseValue::Bytes(data) => Resp3Frame::BulkString(data.clone()),
        ResponseValue::Null => Resp3Frame::Null,
        ResponseValue::Map(map) => response_map_to_frame(map),
        ResponseValue::Array(items) => {
            Resp3Frame::Array(items.iter().map(response_value_to_frame).collect())
        }
        ResponseValue::Json(val) => {
            Resp3Frame::BulkString(serde_json::to_string(val).unwrap_or_default().into_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandError;
    use crate::response::ResponseMap;

    #[test]
    fn serialize_success() {
        let resp = CommandResponse::Success(ResponseMap::ok());
        let frame = response_to_frame(&resp);
        match frame {
            Resp3Frame::Map(pairs) => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, Resp3Frame::BulkString(b"status".to_vec()));
                assert_eq!(pairs[0].1, Resp3Frame::BulkString(b"OK".to_vec()));
            }
            _ => panic!("expected Map frame"),
        }
    }

    #[test]
    fn serialize_error() {
        let err = CommandError::BadArg {
            message: "nope".into(),
        };
        let resp = CommandResponse::Error(err);
        let frame = response_to_frame(&resp);
        match frame {
            Resp3Frame::SimpleError(msg) => {
                assert!(msg.starts_with("bad argument:"));
            }
            _ => panic!("expected SimpleError frame"),
        }
    }
}
