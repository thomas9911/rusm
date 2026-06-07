//! A minimal `wasi:http` component: every request is answered, **by the guest**,
//! with `200 hello from RUSM`. The host (hyper) only moves bytes; this sandboxed
//! component builds the response.

use anyhow::Result;
use wstd::http::body::Body;
use wstd::http::{Request, Response};

#[wstd::http_server]
async fn main(_request: Request<Body>) -> Result<Response<Body>> {
    Ok(Response::new("hello from RUSM\n".to_owned().into()))
}
