use color_eyre::{eyre::bail, Result};
use hyper::{
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    Body, Response, StatusCode,
};
use soup::prelude::*;
use url_path::UrlPath;

/// Returns a list of files parsed from HTML text.
fn list_raw(text: &str) -> Vec<String> {
    Soup::new(text)
        .tag("a")
        .find_all()
        .filter_map(|a| {
            if let Some(href) = a.get("href") {
                let normalized = UrlPath::new(&href).normalize();
                if !normalized.starts_with(['/', '?']) {
                    return Some(normalized);
                }
            }
            None
        })
        .collect::<Vec<_>>()
}

/// Adds extra functionality to `hyper::Response<Body>`.
pub trait ResponseExt {
    /// Returns true if the response has a `Content-Type` header indicating it is HTML.
    fn is_html(&self) -> bool;

    /// Verifies that the response is valid according to various criteria.
    fn verify(&self) -> Result<()>;
}

impl ResponseExt for Response<Body> {
    /// Returns true if the response has a `Content-Type` header indicating it is HTML.
    fn is_html(&self) -> bool {
        self.headers()
            .get(CONTENT_TYPE)
            .map(|content_type| {
                content_type
                    .to_str()
                    .map(|t| t == "text/html")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Verifies that the response is valid according to various criteria.
    fn verify(&self) -> Result<()> {
        let status = self.status();
        if status != StatusCode::OK {
            bail!("Responded with status code {status}");
        }
        if let Some(content_length) = self.headers().get(CONTENT_LENGTH) {
            if content_length.to_str()?.parse::<u16>() == Ok(0) {
                bail!("Responded with content-length equal to zero");
            }
        }

        if self.is_html() {
            bail!("Responded with HTML");
        }
        Ok(())
    }
}

/// Returns a list of files parsed from the HTML in a `hyper::Response<Body>`.
pub async fn list(res: Response<Body>) -> Result<Vec<String>> {
    let body = hyper::body::to_bytes(res).await?;
    let text = std::str::from_utf8(&body)?;
    Ok(list_raw(text))
}
