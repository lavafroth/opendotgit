use crate::{args::Args, expression, response::ResponseExt, webpage};

use color_eyre::eyre::{bail, eyre, Context, Result};
use futures::{stream, StreamExt};
use log::{error, warn};
use reqwest::{header::LOCATION, redirect::Policy, Client, Response, StatusCode};
use std::path::Path;
use tokio::{
    fs,
    time::{timeout, Duration},
};
use tokio_retry::strategy::{jitter, ExponentialBackoff};
use tokio_retry::Retry;
use url::Url;
pub enum Status<'a> {
    Done,
    Follow(&'a str),
}

impl<'a> Status<'a> {
    fn redirect(&self) -> Option<String> {
        // Append a slash to the URL, this is generally the location
        // of the directory index. If the redirect location is different,
        // something is very wrong.
        match self {
            Status::Done => None,
            Status::Follow(href) => Some(format!("{href}/")),
        }
    }
}

pub struct Downloader {
    pub url: Url,
    pub jobs: usize,
    /// The HTTP(S) client used to retrieve content from the repository.
    pub client: Client,
    pub retries: usize,
    pub timeout: Duration,
}

impl From<Args> for Downloader {
    fn from(value: Args) -> Self {
        let mut url = value.url.clone();
        // If there are URL segments, set the new path as the segments upto but not including ".git"
        if let Some(segments) = url.path_segments() {
            url.set_path(
                segments
                    .take_while(|&segment| segment != ".git")
                    .collect::<Vec<_>>()
                    .join("/")
                    .trim_end_matches('/'),
            );
        }
        // If there are no segments, an omitted ".git" segment after the URL is assumed.
        let client = Client::builder().redirect(Policy::none()).build().unwrap();

        Downloader {
            url,
            jobs: value.jobs,
            client,
            retries: value.retries,
            timeout: value.timeout,
        }
    }
}

impl Downloader {
    /// Recursively downloads all files in list.
    pub async fn recursive(&self, links: &[&str]) -> Result<()> {
        // First run through the links supplied
        let mut redirects: Vec<String> = self.collect_links_multiple(links).await;
        while !redirects.is_empty() {
            // Download each file in the list concurrently up to the specified number of jobs.
            redirects = self.collect_links_multiple(&redirects).await;
        }
        Ok(())
    }

    pub async fn collect_links(&self, href: &str) -> Result<Vec<String>> {
        let response = self.fetch(href).await?;
        if !response.is_html() {
            warn!(
                "{}{} responded without content type text/html",
                self.url, href
            );
        }
        Ok(webpage::list(response)
            .await?
            .into_iter()
            .map(|child| format!("{href}/{child}"))
            .collect())
    }

    pub async fn collect_links_multiple<S: AsRef<str>>(&self, sources: &[S]) -> Vec<String> {
        stream::iter(self.multiple(sources).await)
            .filter_map(|s| async move { s.redirect() })
            .map(|href| async move { self.collect_links(&href).await })
            .buffer_unordered(self.jobs)
            .filter_map(|b| async { b.map_err(|e| error!("Failed to fetch resource: {e}")).ok() })
            .flat_map(stream::iter)
            .collect()
            .await
    }

    pub fn normalize_url(&self, href: &str) -> Result<url::Url> {
        let mut url = self.url.clone();
        // Merge the segments of the URL with the segments in href to create the correct URL for the resource.
        let segments: Vec<&str> = url
            .path_segments()
            .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
            .chain(href.split('/'))
            .collect();
        url.set_path(&segments.join("/"));
        Ok(url.as_str().parse()?)
    }

    pub async fn fetch_raw_url(&self, uri: &url::Url) -> Result<Response> {
        let uri = uri.clone();
        let retry_strategy = ExponentialBackoff::from_millis(10)
            .map(jitter)
            .take(self.retries);

        let retry_future = Retry::spawn(retry_strategy, || async {
            self.client.get(uri.clone()).send().await
        });
        Ok(timeout(self.timeout, retry_future).await??)
    }

    /// Returns the response from retrieving a resource at href.
    pub async fn fetch(&self, href: &str) -> Result<Response> {
        self.fetch_raw_url(&self.normalize_url(href)?).await
    }

    /// Downloads a single file at href.
    pub async fn single<'a>(&self, href: &'a str) -> Result<Status<'a>> {
        let res = self.fetch(href).await?;
        let url = &self.url;
        let status = res.status();
        match status {
            // If the status code is one of these, it is a directory.
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                return Ok(Status::Follow(href));
            }
            StatusCode::OK => {
                // Write the contents of the response to disk.
                if res.is_html() {
                    warn!("{url}{href} responded with HTML, probably not found");
                } else {
                    self.write_bytes(href, &res.bytes().await?)
                        .await
                        .context(format!("unable to write bytes for {url}{href}"))?;
                }
            }
            _ => {
                warn!("{url}{href} responded with status code {status}");
            }
        }
        Ok(Status::Done)
    }

    /// Downloads all files in list.
    pub async fn multiple<'a, S: AsRef<str>>(&self, list: &'a [S]) -> Vec<Status<'a>> {
        // Download each file in the list concurrently up to the specified number of jobs.
        stream::iter(list)
            .map(|href| self.single(href.as_ref()))
            .buffer_unordered(self.jobs)
            .filter_map(|b| async {
                b.map_err(|e| error!("Failed while fetching resource: {e}"))
                    .ok()
            })
            .collect::<Vec<_>>()
            .await
    }

    /// Writes the body to a file after creating the parent directory if it doesn't exist already.
    async fn write_bytes<P: AsRef<Path>>(&self, path: P, body: &[u8]) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).await?;
            fs::write(path, body).await?;
            Ok(())
        } else {
            bail!("Parent directory unavailable");
        }
    }

    /// Finds all references from the given href and returns them as a vector of strings.
    async fn refs<S: AsRef<str>>(&self, href: S) -> Result<Vec<String>> {
        let mut href = href.as_ref().to_string();
        let text = loop {
            let response = self.fetch(&href).await?;
            let status = response.status();
            match status {
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                    if let Some(loc) = response.headers().get(LOCATION) {
                        href = loc.to_str()?.to_string();
                    }
                }
                StatusCode::OK => break response.text().await?,
                _ => bail!("{href} returned status code {status}"),
            }
        };

        self.write_bytes(href, &text.as_bytes()).await?;
        Ok(expression::REFS
            .captures_iter(&text)
            .filter_map(|matched| matched.get(0))
            .map(|reference| reference.as_str())
            /* TODO: .filter(is_safe_path(reference)) */
            .flat_map(|reference| {
                vec![
                    format!(".git/{reference}"),
                    format!(".git/logs/{reference}"),
                ]
            })
            .collect::<Vec<_>>())
    }

    async fn refs_multiple<S: AsRef<str>>(&self, refs: &[S]) -> Vec<String> {
        stream::iter(refs)
            .map(|href| self.refs(href))
            .buffer_unordered(self.jobs)
            .filter_map(|b| async {
                b.map_err(|e| error!("Failed while fetching reference: {e}"))
                    .ok()
            })
            .flat_map(stream::iter) // Essentially a .flatten()
            .collect::<Vec<_>>()
            .await
    }

    /// Finds all references recursively from a given list and returns them.
    pub async fn refs_recursive(&self, list: &[&str]) {
        let mut branches = self.refs_multiple(list).await;
        while !branches.is_empty() {
            branches = self.refs_multiple(&branches).await;
        }
    }
}
