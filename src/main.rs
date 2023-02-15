use clap::Parser;
use color_eyre::{eyre::eyre, eyre::WrapErr, Section};

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
    if let Some(content_type) = response.headers().get(CONTENT_TYPE) {
        if content_type == "text/html" {
            Err(eyre!("{url} responded with HTML"))?
        }
    }
    Ok(())
}

fn parse_url(url: &Url) -> Url {
    let mut url = url.clone();
    // If there are segments in the URL,
    // check if any of them equal ".git"
    if let Some(segments) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
        // Find the last ".git" path segment and use the URL till
        // that segment without including it.
        if let Some(index) = segments.iter().position(|&segment| segment == ".git") {
            println!("{}", index);
            url.set_path(&segments[0..index].join("/"));
        }
        // If there were no ".git" segments, an omitted ".git" segment after
        // the given path is assumed.
    };
    // If there are no segments, an omitted ".git" segment after the URL is assumed.
    url
}

async fn fetch_head(url: &Url) -> Result<()> {
    let mut url = parse_url(url);
    url.set_path(&format!("{}/.git/HEAD", url.path()));
    let client = reqwest::Client::new();
    let res = client.get(url).send().await?;
    verify_response(&res)?;
    println!("{:#?}", res.text().await?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.output)
        .wrap_err("Failed to create output directory")
        .suggestion("Try supplying a location you can write to")?;
    fetch_head(&cli.url).await?;
    Ok(())
}
