use std::io::Cursor;

use clap::{ArgAction::Count, Parser};
use color_eyre::{eyre::eyre, eyre::WrapErr, Section};
use futures::{stream, StreamExt};
use hyper::{
    client::{connect::dns::GaiResolver, HttpConnector},
    header::{CONTENT_LENGTH, CONTENT_TYPE},
    Body, Client, Response, StatusCode,
};
use hyper_tls::HttpsConnector;
use regex::Regex;
use soup::prelude::*;
use url_path::UrlPath;

use color_eyre::eyre::Result;
use url::Url;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// URL of the .git directory
    url: Url,

    /// Directory to output the results
    output: String,

    /// Number of asynchronous jobs to spawn
    #[arg(short, long, default_value_t = 8)]
    tasks: usize,

    /// Turn debugging information on
    #[arg(short, long, action = Count)]
    verbose: u8,
}

macro_rules! string_vec {
    ($($x:expr),*) => (vec![$($x.to_string()),*]);
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

fn get_indexed_files(text: &str) -> Vec<String> {
    Soup::new(text)
        .tag("a")
        .find_all()
        .filter_map(|a| match a.get("href") {
            Some(href) => {
                let normalized = UrlPath::new(&href).normalize();
                if normalized.starts_with(['/', '?']) {
                    None
                } else {
                    Some(normalized)
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>()
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

async fn recursive_download(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    list: Vec<String>,
    n_tasks: usize,
) -> Result<()> {
    let mut list = list;
    while !list.is_empty() {
        list = stream::iter(list)
            .map(|href| {
                let client = &client;
                download(client, url, href)
            })
            .buffer_unordered(n_tasks)
            .filter_map(|b| async {
                match b {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Failed while fetching resource: {e}");
                        None
                    }
                }
            })
            // Pretty sure there's a better way to do this.
            // TODO: somehow use flatmap
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
    }
    Ok(())
}

async fn get(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    href: &str,
) -> Result<Response<Body>> {
    let mut url = url.clone();
    let mut segments = url
        .path_segments()
        .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
        .collect::<Vec<_>>();
    segments.push(href);
    url.set_path(&segments.join("/"));
    let response = client.get(url.as_str().parse()?).await?;
    Ok(response)
}

async fn download(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    href: String,
) -> Result<Option<Vec<String>>> {
    let res = get(client, url, &href).await?;
    let status = res.status();
    match status {
        // If the status code is one of these, it is a directory
        StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
            // Append a slash to the URL, this is generally the location
            // of the directory index. If the redirect location is different,
            // something is very wrong.
            let res = get(client, url, "").await?;
            std::fs::create_dir(&href)?;

            if !is_html(&res) {
                println!("warn: {url} responded without Content-Type: text/html");
            }

            Ok(Some(
                get_indexed_files(std::str::from_utf8(&hyper::body::to_bytes(res).await?)?)
                    .into_iter()
                    .map(|child| format!("{href}/{child}"))
                    .collect(),
            ))
        }
        StatusCode::OK => {
            let mut file = std::fs::File::create(href)?;
            let body = &hyper::body::to_bytes(res).await?;
            let mut content = Cursor::new(&body);
            std::io::copy(&mut content, &mut file)?;
            Ok(None)
        }
        _ => {
            println!("warn: {url} responded with status code {status}");
            Ok(None)
        }
    }
}

async fn download_all(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    list: Vec<String>,
    n_tasks: usize,
) -> Result<()> {
    stream::iter(list)
        .map(|href| download(client, url, href))
        .buffer_unordered(n_tasks)
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

async fn find_refs(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    href: &str,
) -> Result<Vec<String>> {
    let res = get(client, url, href).await?;
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

async fn find_all_refs(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    list: Vec<String>,
    n_tasks: usize,
) -> Result<()> {
    let mut list = list;
    while !list.is_empty() {
        list = stream::iter(list)
            .map(|href| async move {
                let href = &href;
                find_refs(client, url, href).await
            })
            .buffer_unordered(n_tasks)
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
        println!("references: {:#?}", list);
    }

    Ok(())
}

async fn fetch_git(mut url: Url, n_tasks: usize) -> Result<()> {
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

    let orig_url = url;
    let mut url = orig_url.clone();
    let mut segments = url
        .path_segments()
        .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
        .collect::<Vec<_>>();
    segments.push(".git");
    segments.push("HEAD");
    url.set_path(&segments.join("/"));
    let url_string = url.to_string();

    let client = Client::builder().build::<_, hyper::Body>(HttpsConnector::new());
    let res = client.get(url_string.parse()?).await?;
    verify_response(&res).wrap_err(format!("While fetching {url}"))?;

    // TODO: consider making this lazy_static
    let ex = r#"^(ref:.*|[0-9a-f]{40}$)"#;
    let re = Regex::new(ex)?;
    let body = hyper::body::to_bytes(res).await?;
    let text = std::str::from_utf8(&body)?;
    if !re.is_match(text.trim()) {
        Err(eyre!("{} is not a git HEAD", url.as_str()))?
    }

    let mut url = url.clone();
    let mut segments = url
        .path_segments()
        .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
        .collect::<Vec<_>>();
    segments.pop();
    url.set_path(&segments.join("/"));
    let url_string = format!("{url}/");

    println!("Testing {}", url_string);

    let res = client.get(url_string.parse()?).await?;
    if !is_html(&res) {
        println!("warn: {url_string} responded without Content-Type: text/html")
    }

    let mut ignore_errors = true;
    let body = hyper::body::to_bytes(res).await?;
    let text = std::str::from_utf8(&body)?;
    let list = get_indexed_files(text);
    if list.iter().any(|filename| filename == "HEAD") {
        ignore_errors = false;
        println!("Recursively downloading {url_string}");
        let tasks: Vec<String> = string_vec![".git", ".gitignore"];
        recursive_download(&client, &orig_url, tasks, n_tasks).await?;
    } else {
        println!("Fetching common files");
        let common_files = string_vec![
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
        ];
        download_all(&client, &orig_url, common_files, n_tasks).await?;
        println!("Finding refs");
        let refs = string_vec![
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
        ];
        find_all_refs(&client, &orig_url, refs, n_tasks).await?;
        println!("Finding packs");
        // TODO:
        // read .git/objects/info/packs if exists
        //   for every sha1 hash, download .git/objects/pack/pack-%s.{idx,pack}
        println!("Finding objects");
        // TODO:
        // For the contents of .git/packed-refs, .git/info/refs, .git/refs/*, .git/logs/*
        //   check if they match "(^|\s)([a-f0-9]{40})($|\s)" and get the second (1) match group
        //
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

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let output = cli.output;
    std::fs::create_dir_all(&output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;
    println!("Changing current directory to \"{output}\"",);
    std::env::set_current_dir(output)?;
    fetch_git(cli.url.clone(), cli.tasks).await?;
    Ok(())
}
