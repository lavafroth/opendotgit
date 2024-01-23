use color_eyre::{eyre::bail, Result};
use reqwest::{
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    Response, StatusCode,
};
/// Adds extra functionality to `hyper::Response<Body>`.
pub trait ResponseExt {
    /// Returns true if the response has a `Content-Type` header indicating it is HTML.
    fn is_html(&self) -> bool;

    /// Verifies that the response is valid according to various criteria.
    fn verify(&self) -> Result<()>;
}

impl ResponseExt for Response {
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
