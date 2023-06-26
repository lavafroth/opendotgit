use crate::{
    constants, expression, pack, string_vec,
    webpage::{self, ResponseExt},
};

use color_eyre::{
    eyre::{bail, eyre, Result, WrapErr},
    Section,
};
use futures::{stream, StreamExt};
use hyper::{
    body::Bytes,
    client::{connect::dns::GaiResolver, HttpConnector},
    Body, Client, Response, StatusCode,
};
use hyper_tls::HttpsConnector;
use log::{error, info, warn};
use pathbuf::pathbuf;
use std::{
    collections::HashSet,
    io::Cursor,
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
    retries: usize,
    timeout: Duration,
}

impl Runner {
    /// Recursively downloads all files in list.
    async fn recursive_download(&self, mut list: Vec<String>) -> Result<()> {
        while !list.is_empty() {
            // Download each file in the list concurrently up to the specified number of jobs.
            list = stream::iter(list)
                .map(|href| self.download(href))
                .buffer_unordered(self.jobs)
                .flat_map(|b| {
                    stream::iter(b.unwrap_or_else(|e| {
                        warn!("Failed while fetching resource: {e}");
                        vec![]
                    }))
                })
                .collect()
                .await;
        }
        Ok(())
    }

    /// Returns the response from retrieving a resource at href.
    async fn get(&self, href: &str) -> Result<Response<Body>> {
        let mut url = self.url.clone();
        // Merge the segments of the URL with the segments in href to create the correct URL for the resource.
        let mut segments = url
            .path_segments()
            .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
            .collect::<Vec<_>>();
        segments.append(&mut href.split('/').collect());
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
    async fn download(&self, href: String) -> Result<Vec<String>> {
        let res = self.get(&href).await?;
        let url = &self.url;
        let status = res.status();
        match status {
            // If the status code is one of these, it is a directory.
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                // Append a slash to the URL, this is generally the location
                // of the directory index. If the redirect location is different,
                // something is very wrong.
                let response = self.get(&format!("{href}/")).await?;
                fs::create_dir_all(&href).await?;

                if !response.is_html() {
                    warn!("{url}{href} responded without content type text/html");
                }

                // Return a list of child resources in the directory.
                Ok(webpage::list(response)
                    .await?
                    .into_iter()
                    .map(|child| format!("{href}/{child}"))
                    .collect())
            }
            StatusCode::OK => {
                // Write the contents of the response to disk.
                self.write_and_yield(Path::new(&href), res).await?;
                Ok(vec![])
            }
            _ => {
                warn!("{url}{href} responded with status code {status}");
                Ok(vec![])
            }
        }
    }

    /// Downloads all files in list.
    async fn download_all(&self, list: Vec<String>) -> Result<()> {
        // Download each file in the list concurrently up to the specified number of jobs.
        stream::iter(list)
            .map(|href| self.download(href))
            .buffer_unordered(self.jobs)
            .map(|b| async { b.map_err(|e| error!("Failed while fetching resource: {e}")) })
            .collect::<Vec<_>>()
            .await;
        Ok(())
    }

    /// Creates a new Runner instance with a given URL and number of jobs.
    pub fn new(url: &Url, jobs: usize, retries: usize, timeout: Duration) -> Runner {
        let mut url = url.clone();
        // If there are URL segments, set the new path as the segments upto but not including ".git"
        if let Some(normalized) = url.path_segments().map(|split| {
            split
                .take_while(|&segment| segment != ".git")
                .collect::<Vec<_>>()
                .join("/")
        }) {
            url.set_path(normalized.trim_end_matches('/'))
        };
        // If there are no segments, an omitted ".git" segment after the URL is assumed.

        Runner {
            url,
            jobs,
            client: Client::builder().build::<_, Body>(HttpsConnector::new()),
            retries,
            timeout,
        }
    }

    /// Writes the response body to a file and returns it as bytes after creating the parent directory
    /// if it doesn't exist already.
    async fn write_and_yield<P: AsRef<Path>>(
        &self,
        path: P,
        response: Response<Body>,
    ) -> Result<Bytes> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).await?;
        } else {
            bail!("Parent directory unavailable");
        }
        let mut file = std::fs::File::create(path)?;
        let body = hyper::body::to_bytes(response).await?;
        let mut content = Cursor::new(&body);
        std::io::copy(&mut content, &mut file)?;
        Ok(body)
    }

    /// Finds all references from the given href and returns them as a vector of strings.
    async fn find_refs(&self, href: &str) -> Result<Vec<String>> {
        let response = self.get(href).await?;
        let body = self.write_and_yield(Path::new(href), response).await?;
        let text = std::str::from_utf8(&body)?;
        Ok(expression::REFS
            .captures_iter(text)
            .flat_map(|mat| {
                if let Some(reference) = mat.get(0).map(|r| r.as_str()) {
                    if !reference.ends_with('*')
                    /* && is_safe_path(reference) */
                    {
                        return vec![
                            format!(".git/{reference}"),
                            format!(".git/logs/{reference}"),
                        ];
                    }
                }
                vec![]
            })
            .collect::<Vec<_>>())
    }

    /// Finds all references recursively from a given list and returns them.
    async fn find_all_refs(&self, mut list: Vec<String>) -> Result<()> {
        while !list.is_empty() {
            list = stream::iter(list)
                .map(|href| async move { self.find_refs(&href).await })
                .buffer_unordered(self.jobs)
                .filter_map(|b| async {
                    b.map_err(|e| error!("Failed while fetching reference: {e}"))
                        .ok()
                })
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
        }

        Ok(())
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

        let mut ignore_errors = true;
        if webpage::list(response)
            .await?
            .iter()
            .any(|filename| filename == "HEAD")
        {
            ignore_errors = false;
            info!("Recursively downloading {url_string}");
            self.recursive_download(string_vec![".git", ".gitignore"])
                .await?;
        } else {
            info!("Fetching common files");
            self.download_all(
                constants::KNOWN_FILES
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
            .await?;
            info!("Finding refs");
            self.find_all_refs(constants::REF_FILES.iter().map(|s| s.to_string()).collect())
                .await?;

            // read .git/objects/info/packs if exists
            //   for every sha1 hash, download .git/objects/pack/pack-%s.{idx,pack}
            info!("Finding packs");

            let pack_path: PathBuf = pathbuf![".git", "objects", "info", "packs"];
            let mut jobs = vec![];
            if pack_path.exists() {
                for capture in expression::PACK.captures_iter(&fs::read_to_string(pack_path).await?)
                {
                    if let Some(sha1) = capture.get(1) {
                        let sha1 = sha1.as_str();
                        jobs.push(format!(".git/objects/pack/pack-{sha1}.idx"));
                        jobs.push(format!(".git/objects/pack/pack-{sha1}.pack"));
                    }
                }
            }
            self.download_all(jobs).await?;

            // For the contents of .git/packed-refs, .git/info/refs, .git/refs/*, .git/logs/*
            //   check if they match "(^|\s)([a-f0-9]{40})($|\s)" and get the second match group
            info!("Finding objects");
            let mut files: Vec<PathBuf> = vec![
                pathbuf![".git", "packed-refs"],
                pathbuf![".git", "info", "refs"],
                pathbuf![".git", "FETCH_HEAD"],
                pathbuf![".git", "ORIG_HEAD"],
            ];

            let mut refs_and_logs = [pathbuf![".git", "refs"], pathbuf![".git", "logs"]]
                .iter()
                .flat_map(|path| {
                    WalkDir::new(path)
                        .into_iter()
                        .filter_map(|e| {
                            e.ok().and_then(|entry| {
                                if entry.file_type().is_file() {
                                    Some(PathBuf::from(entry.path()))
                                } else {
                                    None
                                }
                            })
                        })
                        .collect::<Vec<PathBuf>>()
                })
                .collect::<Vec<PathBuf>>();

            files.append(&mut refs_and_logs);

            let mut objs = HashSet::new();
            for filepath in files {
                if filepath.exists() {
                    for m in expression::OBJECT.captures_iter(&fs::read_to_string(filepath).await?)
                    {
                        if let Some(m) = m.get(2) {
                            objs.insert(m.as_str().to_string());
                        }
                    }
                }
            }

            let index = git2::Index::open(&pathbuf![".git", "index"])?;

            for entry in index.iter() {
                let oid = entry.id;
                objs.insert(oid.to_string());
            }

            let pack_file_dir = pathbuf![".git", "objects", "pack"];
            if pack_file_dir.is_dir() {
                let packs: HashSet<_> = WalkDir::new(&pack_file_dir)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy();
                        if entry.file_type().is_file()
                            && name.starts_with("pack-")
                            && name.ends_with(".idx")
                        {
                            Some(pack::parse(entry.path()).unwrap())
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .collect();
                objs = objs.union(&packs).cloned().collect();
            }

            objs.take("0000000000000000000000000000000000000000");

            self.download_all(
                objs.into_iter()
                    .map(|obj| format!(".git/objects/{}/{}", &obj[0..2], &obj[2..]))
                    .collect(),
            )
            .await?;
        }
        info!("Performing a git checkout");
        checkout(ignore_errors)
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
        .section("Some files from the repository's tree may be missing")?
    }
    Ok(())
}
