//! Minimal mock HTTP server for provider tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// A captured request.
#[derive(Debug, Clone)]
pub struct Captured {
    pub method: String,
    pub path_and_query: String,
    pub body: Value,
}

/// A canned response.
#[derive(Debug, Clone)]
pub struct Canned {
    pub status: u16,
    pub body: Value,
    pub headers: Vec<(&'static str, String)>,
}

impl Canned {
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
        }
    }
}

pub struct MockServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Captured>>>,
}

/// Starts a server that answers with `responses` in order (the last one repeats).
pub async fn start(responses: Vec<Canned>) -> MockServer {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(responses)));
    let captured = requests.clone();
    let app = axum::Router::new().fallback(
        move |method: Method, uri: Uri, _headers: HeaderMap, body: Bytes| {
            let captured = captured.clone();
            let queue = queue.clone();
            async move {
                captured.lock().unwrap().push(Captured {
                    method: method.to_string(),
                    path_and_query: uri
                        .path_and_query()
                        .map(|p| p.to_string())
                        .unwrap_or_default(),
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                });
                let canned = {
                    let mut q = queue.lock().unwrap();
                    if q.len() > 1 {
                        q.pop_front().unwrap()
                    } else {
                        q.front().cloned().unwrap()
                    }
                };
                let mut resp: Response = (
                    StatusCode::from_u16(canned.status).unwrap(),
                    axum::Json(canned.body),
                )
                    .into_response();
                for (k, v) in canned.headers {
                    resp.headers_mut().insert(k, v.parse().unwrap());
                }
                resp
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockServer {
        base: format!("http://{addr}"),
        requests,
    }
}
