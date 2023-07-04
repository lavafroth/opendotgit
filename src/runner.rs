use crate::{constants, expression, pack, response::ResponseExt, webpage};

use color_eyre::{
    eyre::{bail, eyre, Result, WrapErr},
    Section,
};
use futures::{stream, StreamExt};
use hyper::{
    body::to_bytes,
    client::{connect::dns::GaiResolver, HttpConnector},
    Body, Client, Response, StatusCode,
};
use hyper_tls::HttpsConnector;
use log::{error, info, warn};
use pathbuf::pathbuf;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::{
    fs,
    time::{timeout, Duration},
};
use tokio_retry::strategy::{jitter, ExponentialBackoff};
use tokio_retry::Retry;
use url::Url;
use walkdir::WalkDir;

/// Represents a Git repository runner.
pub struct Runner {
    /// The URL of the Git repository to run against.
    url: Url,
    /// The number of jobs to execute concurrently.
    jobs: usize,
    /// The HTTP(S) client used to retrieve content from the repository.
    client: Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    /// Number of times to retry a failed request.
    retries: usize,
    /// Timeout before all attempts for a request are cancelled.
    timeout: Duration,
}

enum DownloadStatus<'a> {
    Done,
    Follow(&'a str),
}

impl<'a> DownloadStatus<'a> {
    fn redirect(&self) -> Option<String> {
        // Append a slash to the URL, this is generally the location
        // of the directory index. If the redirect location is different,
        // something is very wrong.
        match self {
            DownloadStatus::Done => None,
            DownloadStatus::Follow(href) => Some(format!("{href}/")),
        }
    }
}

impl Runner {
    /// Recursively downloads all files in list.
    async fn recursive_download(&self, links: &[&str]) -> Result<()> {
        // First run through the links supplied
        let mut redirects: Vec<String> = self.collect_links_multiple(links).await;
        while !redirects.is_empty() {
            // Download each file in the list concurrently up to the specified number of jobs.
            redirects = self.collect_links_multiple(&redirects).await;
        }
        Ok(())
    }

    async fn collect_links(&self, href: &str) -> Result<Vec<String>> {
        let response = self.get(href).await?;
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

    async fn collect_links_multiple<S: AsRef<str>>(&self, sources: &[S]) -> Vec<String> {
        stream::iter(self.download_multiple(sources).await)
            .filter_map(|s| async move { s.redirect() })
            .map(|href| async move { self.collect_links(&href).await })
            .buffer_unordered(self.jobs)
            .filter_map(|b| async { b.map_err(|e| error!("Failed to fetch resource: {e}")).ok() })
            .flat_map(stream::iter)
            .collect()
            .await
    }

    /// Returns the response from retrieving a resource at href.
    async fn get(&self, href: &str) -> Result<Response<Body>> {
        let mut url = self.url.clone();
        // Merge the segments of the URL with the segments in href to create the correct URL for the resource.
        let segments: Vec<&str> = url
            .path_segments()
            .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
            .chain(href.split('/'))
            .collect();
        url.set_path(&segments.join("/"));
        let url_str: hyper::Uri = url.as_str().parse()?;

        let retry_strategy = ExponentialBackoff::from_millis(10)
            .map(jitter)
            .take(self.retries);

        let retry_future = Retry::spawn(retry_strategy, || async {
            self.client.get(url_str.clone()).await
        });
        Ok(timeout(self.timeout, retry_future).await??)
    }

    /// Downloads a single file at href.
    async fn download<'a>(&self, href: &'a str) -> Result<DownloadStatus<'a>> {
        let res = self.get(href).await?;
        let url = &self.url;
        let status = res.status();
        match status {
            // If the status code is one of these, it is a directory.
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                return Ok(DownloadStatus::Follow(href));
            }
            StatusCode::OK => {
                // Write the contents of the response to disk.
                self.write_bytes(href, &to_bytes(res).await?).await?;
            }
            _ => {
                warn!("{url}{href} responded with status code {status}");
            }
        }
        Ok(DownloadStatus::Done)
    }

    /// Downloads all files in list.
    async fn download_multiple<'a, S: AsRef<str>>(&self, list: &'a [S]) -> Vec<DownloadStatus<'a>> {
        // Download each file in the list concurrently up to the specified number of jobs.
        stream::iter(list)
            .map(|href| self.download(href.as_ref()))
            .buffer_unordered(self.jobs)
            .filter_map(|b| async {
                b.map_err(|e| error!("Failed while fetching resource: {e}"))
                    .ok()
            })
            .collect::<Vec<_>>()
            .await
    }

    /// Creates a new Runner instance with a given URL and number of jobs.
    pub fn new(url: &Url, jobs: usize, retries: usize, timeout: Duration) -> Runner {
        let mut url = url.clone();
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

        Runner {
            url,
            jobs,
            client: Client::builder().build::<_, Body>(HttpsConnector::new()),
            retries,
            timeout,
        }
    }

    /// Writes the body to a file after creating the parent directory if it doesn't exist already.
    async fn write_bytes<P: AsRef<Path>>(&self, path: P, body: &[u8]) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).await?;
            fs::write(path, body).await?;
        }
        bail!("Parent directory unavailable");
    }

    /// Finds all references from the given href and returns them as a vector of strings.
    async fn find_refs<S: AsRef<str>>(&self, href: S) -> Result<Vec<String>> {
        let href = href.as_ref();
        let response = self.get(href).await?;
        let body = to_bytes(response).await?;
        self.write_bytes(href, &body).await?;
        let text = std::str::from_utf8(&body)?;
        Ok(expression::REFS
            .captures_iter(text)
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

    async fn find_multiple_refs<S: AsRef<str>>(&self, refs: &[S]) -> Vec<String> {
        stream::iter(refs)
            .map(|href| self.find_refs(href))
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
    async fn find_all_refs(&self, list: &[&str]) {
        let mut branches = self.find_multiple_refs(list).await;
        while !branches.is_empty() {
            branches = self.find_multiple_refs(&branches).await;
        }
    }

    /// Runs the Runner instance with the specified parameters and performs a Git checkout.
    pub async fn run(self) -> Result<()> {
        let url = &self.url;
        let response = self.get(".git/HEAD").await?;
        response
            .verify()
            .wrap_err(format!("While fetching {url}"))?;

        let body = hyper::body::to_bytes(response).await?;
        let text = std::str::from_utf8(&body)?;
        if !expression::HEAD.is_match(text.trim()) {
            bail!("{url} is not a git HEAD");
        }
        let path = ".git/";
        let url_string = format!("{url}{path}");
        info!("Testing {url_string}");

        let response = self.get(path).await?;
        if !response.is_html() {
            warn!("{url_string} responded without content type text/html")
        }

        let is_webpage_listing = webpage::list(response)
            .await?
            .iter()
            .any(|filename| filename == "HEAD");
        if is_webpage_listing {
            info!("Recursively downloading {url_string}");
            self.recursive_download(&[".git", ".gitignore"]).await?;
        } else {
            self.manual_search().await?;
        }
        info!("Performing a git checkout");
        checkout(!is_webpage_listing)
    }

    async fn manual_search(&self) -> Result<()> {
        info!("Fetching common files");
        self.download_multiple(constants::KNOWN_FILES).await;
        info!("Finding refs");
        self.find_all_refs(constants::REF_FILES).await;

        // read .git/objects/info/packs if exists
        //   for every sha1 hash, download .git/objects/pack/pack-%s.{idx,pack}
        info!("Finding packs");

        let pack_path: PathBuf = pathbuf![".git", "objects", "info", "packs"];
        let mut jobs = vec![];
        if pack_path.exists() {
            for capture in expression::PACK.captures_iter(&fs::read_to_string(pack_path).await?) {
                if let Some(sha1) = capture.get(1) {
                    let sha1 = sha1.as_str();
                    jobs.push(format!(".git/objects/pack/pack-{sha1}.idx"));
                    jobs.push(format!(".git/objects/pack/pack-{sha1}.pack"));
                }
            }
        }
        self.download_multiple(&jobs).await;

        // For the contents of .git/packed-refs, .git/info/refs, .git/refs/*, .git/logs/*
        //   check if they match "(^|\s)([a-f0-9]{40})($|\s)" and get the second match group
        info!("Finding objects");
        let mut files: Vec<PathBuf> = vec![
            pathbuf![".git", "packed-refs"],
            pathbuf![".git", "info", "refs"],
            pathbuf![".git", "FETCH_HEAD"],
            pathbuf![".git", "ORIG_HEAD"],
        ];

        let search_paths = [pathbuf![".git", "refs"], pathbuf![".git", "logs"]];
        let refs_and_logs = search_paths.iter().flat_map(|path| {
            WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().into())
        });

        files.extend(refs_and_logs);

        let mut objs = HashSet::new();
        for filepath in files {
            let text = fs::read_to_string(filepath).await?;
            let matches = expression::OBJECT
                .captures_iter(&text)
                .filter_map(|m| m.get(2))
                .map(|m| m.as_str().to_string());
            objs.extend(matches);
        }

        let index = git2::Index::open(&pathbuf![".git", "index"])?;
        objs.extend(index.iter().map(|entry| entry.id.to_string()));

        let pack_file_dir = pathbuf![".git", "objects", "pack"];
        if pack_file_dir.is_dir() {
            let packs = WalkDir::new(&pack_file_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|entry| {
                    let name = entry.file_name().to_string_lossy();
                    entry.file_type().is_file()
                        && name.starts_with("pack-")
                        && name.ends_with(".idx")
                })
                .flat_map(|entry| pack::parse(entry.path()).unwrap());
            objs.extend(packs);
        }

        objs.take("0000000000000000000000000000000000000000");

        let object_paths = objs
            .into_iter()
            .map(|obj| format!(".git/objects/{}/{}", &obj[0..2], &obj[2..]))
            .collect::<Vec<_>>();

        self.download_multiple(&object_paths).await;
        Ok(())
    }
}

/// Checks out the Git repository and returns a Result indicating success or failure of the operation.
fn checkout(ignore_errors: bool) -> Result<()> {
    let status = std::process::Command::new("git")
        .arg("checkout")
        .status()
        .wrap_err("Failed to run git checkout")
        .suggestion("Make sure your system has git installed")?;
    if ignore_errors && !status.success() {
        Err(eyre!(
            "Checkout command did not exit cleanly, exit status: {status}"
        ))
        .note("Some files from the repository's tree may be missing")?
    }
    Ok(())
}
