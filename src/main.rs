use std::{io::Cursor, path::PathBuf};

use clap::Parser;
use color_eyre::{eyre::eyre, eyre::WrapErr, Section};
use futures::{stream, StreamExt};
use hyper::{
    client::{connect::dns::GaiResolver, HttpConnector},
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
    output: PathBuf,

    /// Number of asynchronous jobs to spawn
    #[arg(short, long, default_value_t = 8)]
    tasks: usize,

    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    debug: u8,
}

fn verify_response(response: &Response<Body>) -> Result<()> {
    let status = response.status();
    if status != 200 {
        Err(eyre!("Responded with status code {status}"))?
    }
    let content_length = response.headers().get("Content-Length");
    if let Some(l) = content_length {
        if l.to_str()?.parse::<u16>() == Ok(0) {
            Err(eyre!("Responded with content-length equal to zero"))?
        }
    }

    if is_html(response) {
        return Err(eyre!("Responded with HTML"));
    }
    Ok(())
}

fn is_html(response: &Response<Body>) -> bool {
    if let Some(content_type) = response.headers().get("Content-Type") {
        if let Ok(ct_type) = content_type.to_str() {
            if ct_type == "text/html" {
                return true;
            }
        }
    }
    false
}

fn normalize_url(url: &mut Url) {
    // If there are segments in the URL,
    // check if any of them equal ".git"
    if let Some(segments) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
        // Find the last ".git" path segment and use the URL till
        // that segment without including it.
        let path = if let Some(index) = segments.iter().position(|&segment| segment == ".git") {
            segments[0..index].join("/")
        } else {
            String::from(url.path())
        };
        url.set_path(path.trim_end_matches('/'));
        // If there were no ".git" segments, an omitted ".git" segment after
        // the given path is assumed.
    };
    // If there are no segments, an omitted ".git" segment after the URL is assumed.
}
fn get_indexed_files(text: &str) -> Vec<String> {
    let soup = Soup::new(text);
    soup.tag("a")
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

async fn fetch_head(mut url: Url, output: &PathBuf, n_tasks: usize) -> Result<()> {
    normalize_url(&mut url);

    let mut url = url.clone();
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

    let re = Regex::new(r"^(ref:.*|[0-9a-f]{40}$)")?;
    let text = hyper::body::to_bytes(res).await?;
    if !re.is_match(std::str::from_utf8(&text)?.trim()) {
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

    let list = get_indexed_files(std::str::from_utf8(&hyper::body::to_bytes(res).await?)?);
    if list.iter().any(|filename| filename == "HEAD") {
        println!("Recursively downloading {url_string}");
        recursive_download(client, &url, list, n_tasks).await?;
    }
    println!(
        "Changing current directory to \"{}\"",
        output.to_str().unwrap()
    );
    std::env::set_current_dir(output)?;
    println!("Performing a git checkout");
    Ok(())
}

async fn recursive_download(
    client: Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
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
    println!("Time to checkout baby!");
    Ok(())
}

async fn download(
    client: &Client<HttpsConnector<HttpConnector<GaiResolver>>, Body>,
    url: &Url,
    href: String,
) -> Result<Option<Vec<String>>> {
    let mut url = url.clone();
    let mut segments = url
        .path_segments()
        .ok_or_else(|| eyre!("Supplied URL cannot be an absolute URL"))?
        .collect::<Vec<_>>();
    segments.push(&href);
    url.set_path(&segments.join("/"));

    let res = client.get(url.as_str().parse()?).await?;
    match res.status() {
        // If the status code is one of these, it is a directory
        StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
            // Append a slash to the URL, this is generally the location
            // of the directory index. If the redirect location is different,
            // something is very wrong.
            let url = format!("{}/", url);
            std::fs::create_dir(&href)?;

            let res = client.get(url.parse()?).await?;
            if !is_html(&res) {
                println!("warn: {url} responded without Content-Type: text/html")
            }

            Ok(Some(
                get_indexed_files(std::str::from_utf8(&hyper::body::to_bytes(res).await?)?)
                    .into_iter()
                    .map(|child| format!("{href}/{child}"))
                    .collect(),
            ))
        }
        _ => {
            let mut file = std::fs::File::create(href)?;
            let body = &hyper::body::to_bytes(res).await?;
            let mut content = Cursor::new(&body);
            std::io::copy(&mut content, &mut file)?;
            Ok(None)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let output = &cli.output;
    let mut dotgit = output.clone();
    dotgit.push(".git");
    std::fs::create_dir_all(&dotgit)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;
    println!(
        "Changing current directory to \"{}\"",
        dotgit.to_str().unwrap()
    );
    std::env::set_current_dir(dotgit)?;
    fetch_head(cli.url.clone(), output, cli.tasks).await?;
    Ok(())
}
