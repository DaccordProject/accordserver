pub mod application;
pub mod attachment;
pub mod channel;
pub mod embed;
pub mod emoji;
pub mod interaction;
pub mod invite;
pub mod member;
pub mod message;
pub mod permission;
pub mod presence;
pub mod role;
pub mod soundboard;
pub mod space;
pub mod user;
pub mod voice;

use serde::Serialize;

/// Standard envelope for single-resource responses.
#[derive(Debug, Serialize)]
pub struct DataResponse<T: Serialize> {
    pub data: T,
}

/// Standard envelope for paginated list responses.
#[derive(Debug, Serialize)]
pub struct ListResponse<T: Serialize> {
    pub data: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Serialize)]
pub struct Cursor {
    pub after: String,
    pub has_more: bool,
}

/// Standard envelope for error responses.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl<T: Serialize> DataResponse<T> {
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T: Serialize> ListResponse<T> {
    pub fn new(data: Vec<T>, after: Option<String>, has_more: bool) -> Self {
        Self {
            data,
            cursor: if after.is_some() || has_more {
                Some(Cursor {
                    after: after.unwrap_or_default(),
                    has_more,
                })
            } else {
                None
            },
        }
    }
}
