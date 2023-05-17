use crate::constants;
use crate::expression;
use crate::pack;
use crate::string_vec;
use crate::webpage;
use hyper::body::Bytes;

use color_eyre::{
    eyre::{bail, eyre, Result, WrapErr},
    Section,
};
use futures::{stream, StreamExt};
use hyper::{
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
use tokio::fs;
use url::Url;
use walkdir::WalkDir;
use webpage::ResponseExt;

pub struct Runner {
    url: Url,
    tasks: usize,
    client: Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
}

impl Runner {
    async fn recursive_download(&self, mut list: Vec<String>) -> Result<()> {
        while !list.is_empty() {
            list = stream::iter(list)
                .map(|href| self.download(href))
                .buffer_unordered(self.tasks)
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

    async fn get(&self, href: &str) -> Result<Response<Body>> {
        let mut url = self.url.clone();
        let mut segments = url
            .path_segments()
            .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
            .collect::<Vec<_>>();
        segments.append(&mut href.split('/').collect());
        url.set_path(&segments.join("/"));
        Ok(self.client.get(url.as_str().parse()?).await?)
    }

    async fn download(&self, href: String) -> Result<Vec<String>> {
        let res = self.get(&href).await?;
        let url = &self.url;
        let status = res.status();
        match status {
            // If the status code is one of these, it is a directory
            StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
                // Append a slash to the URL, this is generally the location
                // of the directory index. If the redirect location is different,
                // something is very wrong.
                let response = self.get(&format!("{href}/")).await?;
                fs::create_dir_all(&href).await?;

                if !response.is_html() {
                    warn!("{url}{href} responded without content type text/html");
                }

                Ok(webpage::list(response)
                    .await?
                    .into_iter()
                    .map(|child| format!("{href}/{child}"))
                    .collect())
            }
            StatusCode::OK => {
                self.write_and_yield(Path::new(&href), res).await?;
                Ok(vec![])
            }
            _ => {
                warn!("{url}{href} responded with status code {status}");
                Ok(vec![])
            }
        }
    }

    async fn download_all(&self, list: Vec<String>) -> Result<()> {
        stream::iter(list)
            .map(|href| self.download(href))
            .buffer_unordered(self.tasks)
            .map(|b| async {
                if let Err(e) = b {
                    error!("Failed while fetching resource: {e}");
                }
            })
            .collect::<Vec<_>>()
            .await;
        Ok(())
    }
    pub fn new(url: &Url, tasks: usize) -> Runner {
        let mut url = url.clone();
        // If there are segments in the URL,
        // check if any of them equal ".git"
        if let Some(segments) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
            // Find the last ".git" path segment and use the URL till
            // that segment without including it.
            url.set_path(
                segments
                    .iter()
                    .position(|&segment| segment == ".git")
                    .map(|index| segments[0..index].join("/"))
                    .unwrap_or(String::from(url.path()))
                    .trim_end_matches('/'),
            );
            // If there were no ".git" segments, an omitted ".git" segment after
            // the given path is assumed.
        };
        // If there are no segments, an omitted ".git" segment after the URL is assumed.

        Runner {
            url,
            tasks,
            client: Client::builder().build::<_, Body>(HttpsConnector::new()),
        }
    }

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

    async fn find_refs(&self, href: &str) -> Result<Vec<String>> {
        let response = self.get(href).await?;
        let body = self.write_and_yield(Path::new(href), response).await?;
        let text = std::str::from_utf8(&body)?;
        Ok(expression::REFS
            .captures_iter(text)
            .filter_map(|mat| {
                if let Some(reference) = mat.get(0) {
                    let reference = reference.as_str();
                    if !reference.ends_with('*')
                    /* && is_safe_path(reference) */
                    {
                        return Some(vec![
                            format!(".git/{reference}"),
                            format!(".git/logs/{reference}"),
                        ]);
                    }
                }
                None
            })
            .flatten()
            .collect::<Vec<_>>())
    }

    async fn find_all_refs(&self, mut list: Vec<String>) -> Result<()> {
        while !list.is_empty() {
            list = stream::iter(list)
                .map(|href| async move {
                    let href = &href;
                    self.find_refs(href).await
                })
                .buffer_unordered(self.tasks)
                .filter_map(|b| async {
                    match b {
                        Ok(b) => Some(b),
                        Err(e) => {
                            error!("Failed while fetching reference: {e}");
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
        }

        Ok(())
    }

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
            let mut tasks = vec![];
            if pack_path.exists() {
                for capture in expression::PACK.captures_iter(&fs::read_to_string(pack_path).await?)
                {
                    if let Some(sha1) = capture.get(1) {
                        let sha1 = sha1.as_str();
                        tasks.push(format!(".git/objects/pack/pack-{sha1}.idx"));
                        tasks.push(format!(".git/objects/pack/pack-{sha1}.pack"));
                    }
                }
            }
            self.download_all(tasks).await?;

            info!("Finding objects");
            // For the contents of .git/packed-refs, .git/info/refs, .git/refs/*, .git/logs/*
            //   check if they match "(^|\s)([a-f0-9]{40})($|\s)" and get the second match group
            //
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
                    .filter_map(|entry| {
                        entry.ok().and_then(|entry| {
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
        .suggestion("Some files from the repository's tree may be missing")?
    }
    Ok(())
}
