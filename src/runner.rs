use crate::parsing;
use color_eyre::{
    eyre::{eyre, Result, WrapErr},
    Section,
};
use futures::{stream, StreamExt};
use hyper::{
    client::{connect::dns::GaiResolver, HttpConnector},
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    Body, Client, Response, StatusCode,
};
use hyper_tls::HttpsConnector;
use regex::Regex;
use std::{io::Cursor, path::PathBuf};
use tokio::fs;
use url::Url;
use walkdir::WalkDir;

macro_rules! string_vec {
    ($($x:expr),*) => (vec![$($x.to_string()),*]);
}
pub struct Runner {
    url: Url,
    tasks: usize,
    client: Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
}

impl Runner {
    async fn recursive_download(&self, list: Vec<String>) -> Result<()> {
        let mut list = list;
        while !list.is_empty() {
            list = stream::iter(list)
                .map(|href| self.download(href))
                .buffer_unordered(self.tasks)
                .flat_map(|b| {
                    stream::iter(b.unwrap_or_else(|e| {
                        eprintln!("Failed while fetching resource: {e}");
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
                let res = self.get(&format!("{href}/")).await?;
                std::fs::create_dir(&href)?;

                if !is_html(&res) {
                    println!("warn: {url}{href} responded without Content-Type: text/html");
                }

                Ok(parsing::list_from_response(res)
                    .await?
                    .into_iter()
                    .map(|child| format!("{href}/{child}"))
                    .collect())
            }
            StatusCode::OK => {
                let mut file = std::fs::File::create(href)?;
                let body = &hyper::body::to_bytes(res).await?;
                let mut content = Cursor::new(&body);
                std::io::copy(&mut content, &mut file)?;
                Ok(vec![])
            }
            _ => {
                println!("warn: {url} responded with status code {status}");
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
                    eprintln!("Failed while fetching resource: {e}");
                }
            })
            // Pretty sure there's a better way to do this.
            // TODO: somehow use flatma
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

    async fn find_refs(&self, href: &str) -> Result<Vec<String>> {
        let res = self.get(href).await?;
        let mut file = std::fs::File::create(href)?;
        let body = &hyper::body::to_bytes(res).await?;
        let mut content = Cursor::new(&body);
        std::io::copy(&mut content, &mut file)?;
        let text = std::str::from_utf8(body)?;
        let ex = r#"(refs(/[a-zA-Z0-9\-\.\_\*]+)+)"#;
        let re = Regex::new(ex)?;
        Ok(re
            .captures_iter(text)
            .filter_map(|mat| {
                if let Some(reference) = mat.get(0) {
                    let reference = reference.as_str();
                    if reference.ends_with('*')
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

    async fn find_all_refs(&self, list: Vec<String>) -> Result<()> {
        let mut list = list;
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
                            eprintln!("Failed while fetching reference: {e}");
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
        let res = self.get(".git/HEAD").await?;
        verify_response(&res).wrap_err(format!("While fetching {url}"))?;

        // TODO: consider making this lazy_static
        let ex = r#"^(ref:.*|[0-9a-f]{40}$)"#;
        let re = Regex::new(ex)?;
        let body = hyper::body::to_bytes(res).await?;
        let text = std::str::from_utf8(&body)?;
        if !re.is_match(text.trim()) {
            Err(eyre!("{url} is not a git HEAD"))?
        }
        let path = ".git/";
        let url_string = format!("{url}{path}");
        println!("Testing {url_string}");

        let res = self.get(path).await?;
        if !is_html(&res) {
            println!("warn: {url_string} responded without Content-Type: text/html")
        }

        let mut ignore_errors = true;
        if parsing::list_from_response(res)
            .await?
            .iter()
            .any(|filename| filename == "HEAD")
        {
            ignore_errors = false;
            println!("Recursively downloading {url_string}");
            self.recursive_download(string_vec![".git", ".gitignore"])
                .await?;
        } else {
            println!("Fetching common files");
            self.download_all(string_vec![
                ".gitignore",
                ".git/COMMIT_EDITMSG",
                ".git/description",
                ".git/hooks/applypatch-msg.sample",
                ".git/hooks/commit-msg.sample",
                ".git/hooks/post-commit.sample",
                ".git/hooks/post-receive.sample",
                ".git/hooks/post-update.sample",
                ".git/hooks/pre-applypatch.sample",
                ".git/hooks/pre-commit.sample",
                ".git/hooks/pre-push.sample",
                ".git/hooks/pre-rebase.sample",
                ".git/hooks/pre-receive.sample",
                ".git/hooks/prepare-commit-msg.sample",
                ".git/hooks/update.sample",
                ".git/index",
                ".git/info/exclude",
                ".git/objects/info/packs"
            ])
            .await?;
            println!("Finding refs");
            self.find_all_refs(string_vec![
                ".git/FETCH_HEAD",
                ".git/HEAD",
                ".git/ORIG_HEAD",
                ".git/config",
                ".git/info/refs",
                ".git/logs/HEAD",
                ".git/logs/refs/heads/master",
                ".git/logs/refs/remotes/origin/HEAD",
                ".git/logs/refs/remotes/origin/master",
                ".git/logs/refs/stash",
                ".git/packed-refs",
                ".git/refs/heads/master",
                ".git/refs/remotes/origin/HEAD",
                ".git/refs/remotes/origin/master",
                ".git/refs/stash",
                ".git/refs/wip/wtree/refs/heads/master",
                ".git/refs/wip/index/refs/heads/master"
            ])
            .await?;

            // read .git/objects/info/packs if exists
            //   for every sha1 hash, download .git/objects/pack/pack-%s.{idx,pack}
            println!("Finding packs");

            let pack_path: PathBuf = [".git", "objects", "info", "packs"].iter().collect();
            let mut tasks = vec![];
            if pack_path.exists() {
                let ex = r"pack-([a-f0-9]{40})\.pack";
                let re = Regex::new(ex)?;
                let info_packs = fs::read_to_string(pack_path).await?;
                for capture in re.captures_iter(&info_packs) {
                    if let Some(sha1) = capture.get(0) {
                        let sha1 = sha1.as_str();
                        tasks.push(format!(".git/objects/pack/pack-{sha1}.idx"));
                        tasks.push(format!(".git/objects/pack/pack-{sha1}.pack"));
                    }
                }
            }
            self.download_all(tasks).await?;

            println!("Finding objects");
            // For the contents of .git/packed-refs, .git/info/refs, .git/refs/*, .git/logs/*
            //   check if they match "(^|\s)([a-f0-9]{40})($|\s)" and get the second (1) match group
            //
            let mut files: Vec<PathBuf> = vec![
                [".git", "packed-refs"].iter().collect(),
                [".git", "info", "refs"].iter().collect(),
                [".git", "FETCH_HEAD"].iter().collect(),
                [".git", "ORIG_HEAD"].iter().collect(),
            ];

            let mut refs_and_logs = [[".git", "refs"], [".git", "logs"]]
                .iter()
                .map(|v| {
                    let path = v.iter().collect::<PathBuf>();

                    WalkDir::new(path)
                        .into_iter()
                        .filter_map(|e| {
                            e.ok().and_then(|en| {
                                if en.file_type().is_file() {
                                    Some(PathBuf::from(en.path()))
                                } else {
                                    None
                                }
                            })
                        })
                        .collect::<Vec<PathBuf>>()
                })
                .flatten()
                .collect::<Vec<PathBuf>>();

            files.append(&mut refs_and_logs);

            let mut objs: Vec<String> = vec![];
            let ex = r"(^|\s)([a-f0-9]{40})($|\s)";
            let re = Regex::new(ex)?;
            for filepath in files {
                if filepath.exists() {
                    for m in re.captures_iter(&fs::read_to_string(filepath).await?) {
                        if let Some(m) = m.get(1) {
                            objs.push(m.as_str().to_string());
                        }
                    }
                }
            }

            println!("Objects:\n\n{:#?}", objs);
            // TODO:
            // If .git/index exists
            //   add the object in the entry (again, second index) to our list of objects
            //
            // For all the files in .git/objects/pack that begin with "pack-" or end with ".pack"
            //   add their sha1 hexdigest to our list of objects if not already present
            //   download format!(".git/objects/{}/{}", obj[0..2], obj[2..])
        }
        println!("Performing a git checkout");
        checkout(ignore_errors)
    }
}

fn verify_response(response: &Response<Body>) -> Result<()> {
    let status = response.status();
    if status != StatusCode::OK {
        Err(eyre!("Responded with status code {status}"))?
    }
    if let Some(content_length) = response.headers().get(CONTENT_LENGTH) {
        if content_length.to_str()?.parse::<u16>() == Ok(0) {
            Err(eyre!("Responded with content-length equal to zero"))?
        }
    }

    if is_html(response) {
        Err(eyre!("Responded with HTML"))?
    }
    Ok(())
}

fn is_html(response: &Response<Body>) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .map(|content_type| {
            content_type
                .to_str()
                .map(|t| t == "text/html")
                .unwrap_or(false)
        })
        .unwrap_or(false)
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
