use color_eyre::Result;
use reqwest::Response;
use soup::prelude::*;
use url_path::UrlPath;

/// Returns a list of files parsed from HTML text.
fn list_raw(text: &str) -> Vec<String> {
    Soup::new(text)
        .tag("a")
        .find_all()
        .filter_map(|a| {
            if let Some(href) = a.get("href").map(|href| UrlPath::new(&href).normalize()) {
                if !href.starts_with(['/', '?']) {
                    return Some(href);
                }
            }
            None
        })
        .collect::<Vec<_>>()
}

/// Returns a list of files parsed from the HTML in a `hyper::Response<Body>`.
pub async fn list(res: Response) -> Result<Vec<String>> {
    Ok(list_raw(&res.text().await?))
}
