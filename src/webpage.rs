use color_eyre::Result;
use hyper::Body;
use hyper::Response;
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

/// Returns a list of files parsed from the HTML in a `hyper::Response<Body>`.
pub async fn list(res: Response<Body>) -> Result<Vec<String>> {
    let body = hyper::body::to_bytes(res).await?;
    let text = std::str::from_utf8(&body)?;
    Ok(list_raw(text))
}
