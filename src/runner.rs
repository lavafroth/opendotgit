use crate::{
    args::Args, constants, download::Downloader, expression, pack, response::ResponseExt, webpage,
};

use color_eyre::{
    eyre::{bail, eyre, Result, WrapErr},
    Section,
};
use log::{info, warn};
use pathbuf::pathbuf;
use std::{collections::HashSet, path::PathBuf};
use tokio::fs;
use walkdir::WalkDir;

pub async fn run(args: Args) -> Result<()> {
    let url = args.url.clone();
    let download: Downloader = args.into();

    let uri = download.normalize_url(".git/HEAD")?;
    let response = download.fetch_raw_url(&uri).await?;
    response
        .verify()
        .wrap_err(format!("While fetching {uri}"))?;

    let body = hyper::body::to_bytes(response).await?;
    let text = std::str::from_utf8(&body)?;
    if !expression::HEAD.is_match(text.trim()) {
        bail!("{url} is not a git HEAD");
    }
    let uri = download.normalize_url(".git")?;
    info!("Testing {uri}");

    let response = download.fetch_raw_url(&uri).await?;
    if !response.is_html() {
        warn!("{uri} responded without content type text/html")
    }

    let is_webpage_listing = webpage::list(response)
        .await?
        .iter()
        .any(|filename| filename == "HEAD");
    if is_webpage_listing {
        info!("Recursively downloading {uri}");
        download.recursive(&[".git", ".gitignore"]).await?;
    } else {
        info!("Fetching common files");
        download.multiple(constants::KNOWN_FILES).await;
        info!("Finding refs");
        download.refs_recursive(constants::REF_FILES).await;

        // read .git/objects/info/packs if exists
        //   for every sha1 hash, download .git/objects/pack/pack-%s.{idx,pack}
        info!("Finding packs");

        let pack_path: PathBuf = pathbuf![".git", "objects", "info", "packs"];
        if pack_path.exists() {
            let jobs: Vec<_> = expression::PACK
                .captures_iter(&fs::read_to_string(pack_path).await?)
                .filter_map(|capture| capture.get(1))
                .map(|sha1| sha1.as_str())
                .flat_map(|sha1| {
                    vec![
                        format!(".git/objects/pack/pack-{sha1}.idx"),
                        format!(".git/objects/pack/pack-{sha1}.pack"),
                    ]
                })
                .collect();
            download.multiple(&jobs).await;
        }

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

        download.multiple(&object_paths).await;
    }
    info!("Performing a git checkout");
    checkout(!is_webpage_listing)
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
