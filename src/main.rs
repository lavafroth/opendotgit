use url_path::UrlPath;

use clap::Parser;
use color_eyre::{eyre::eyre, eyre::WrapErr, Section};
use regex::Regex;
use soup::prelude::*;

use color_eyre::eyre::Result;
use reqwest::header::CONTENT_TYPE;
use reqwest::Response;
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
    jobs: u8,

    /// Turn debugging information on
    #[arg(short, long, action = clap::ArgAction::Count)]
    debug: u8,
}

fn verify_response(response: &Response) -> Result<()> {
    let url = response.url().as_str();
    let status = response.status();
    if status != 200 {
        Err(eyre!("{url} responded with status code {status}"))?
    }
    if response.content_length() == Some(0) {
        Err(eyre!("{url} responded with content-length equal to zero"))?
    }
    if is_html(response) {
        Err(eyre!("{url} responded with HTML"))?
    }
    Ok(())
}

fn is_html(response: &Response) -> bool {
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        if content_type == "text/html" {
            return true;
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

async fn fetch_head(mut url: Url) -> Result<()> {
    normalize_url(&mut url);

    url.path_segments_mut()
        .map_err(|_| eyre!("Supplied URL cannot be an absolute URL"))?
        .push(".git")
        .push("HEAD");

    let client = reqwest::Client::new();

    let res = client.get(url.clone()).send().await?;
    verify_response(&res)?;

    let re = Regex::new(r"^(ref:.*|[0-9a-f]{40}$)")?;
    let text = res.text().await?;
    if !re.is_match(text.trim()) {
        Err(eyre!("{} is not a git HEAD", url.as_str()))?
    }

    url.path_segments_mut()
        .map_err(|_| eyre!("Supplied URL cannot be an absolute URL"))?
        .pop();

    println!("Testing {}", url.as_str());

    let res = client.get(url.clone()).send().await?;
    if is_html(&res) {
        println!("warn: {url} responsed without Content-Type: text/html")
    }
    let text = res.text().await?;
    if get_indexed_files(&text)
        .iter()
        .any(|filename| filename == "HEAD")
    {
        println!("nice");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;
    fetch_head(cli.url.clone()).await?;
    Ok(())
}
