use anyhow::{bail, ensure, Context, Result, anyhow, Ok};
use git_url_parse::GitUrl;
use std::{env::current_dir, fs, path::PathBuf, str::FromStr};
use structopt::StructOpt;
use toml_edit::{Document, InlineTable, Value, Item, Table};
use walkdir::{DirEntry, WalkDir};
use reqwest::header::USER_AGENT;
use serde_json;
/// Which dependencies should be rewritten?
#[derive(Debug, Clone)]
enum Rewrite {
    All,
    Substrate(Option<String>),
    Polkadot(Option<String>),
    Cumulus(Option<String>),
    Beefy(Option<String>),
}

/// The different sources `Version` can be generated from.
#[derive(Debug, Clone)]
enum VersionSource {
    CratesIO,
    Url(String),
    File(String),
}


/// The version the dependencies should be switched to.
#[derive(Debug, Clone)]
enum Key {
    Tag(String),
    Branch(String),
    Rev(String),
    Version(VersionSource),
}

/// `update` subcommand options.
#[derive(Debug, StructOpt)]
pub struct Update {
    /// The path where Diener should search for `Cargo.toml` files.
    #[structopt(long)]
    path: Option<PathBuf>,

    /// Only alter Substrate dependencies.
    #[structopt(long, short = "s")]
    substrate: bool,

    /// Only alter Polkadot dependencies.
    #[structopt(long, short = "p")]
    polkadot: bool,

    /// Only alter Cumulus dependencies.
    #[structopt(long, short = "c")]
    cumulus: bool,

    /// Only alter BEEFY dependencies.
    #[structopt(long, short = "b")]
    beefy: bool,

    /// Alter polkadot, substrate + beefy dependencies
    #[structopt(long, short = "a")]
    all: bool,

    /// The `branch` that the dependencies should use.
    #[structopt(long, conflicts_with_all = &[ "rev", "tag", "version" ])]
    branch: Option<String>,

    /// The `rev` that the dependencies should use.
    #[structopt(long, conflicts_with_all = &[ "branch", "tag", "version" ])]
    rev: Option<String>,

    /// The `tag` that the dependencies should use.
    #[structopt(long, conflicts_with_all = &[ "rev", "branch", "version" ])]
    tag: Option<String>,

    /// The `version` source the crates should be updated from.
    #[structopt(long, conflicts_with_all = &[ "git" ])]
    version: Option<String>,

    /// Rewrite the `git` url to the give one.
    #[structopt(long, conflicts_with_all = &[ "version" ])]
    git: Option<String>,
}

impl Update {
    /// Convert the options into the parts `Rewrite`, `Key`, `Option<PathBuf>`.
    fn into_parts(self) -> Result<(Rewrite, Key, Option<PathBuf>)> {
        let key = if let Some(branch) = self.branch {
            Key::Branch(branch)
        } else if let Some(rev) = self.rev {
            Key::Rev(rev)
        } else if let Some(tag) = self.tag {
            Key::Tag(tag)
        } else if let Some(version) = self.version.clone() {
            let source = get_version_source(&version)?;
            Key::Version(source)
        } else {
            bail!("You need to pass `--branch`, `--tag`, `--rev` or `--version`.");
        };

        let rewrite = if self.all || self.version.is_some() {
            if self.git.is_some() {
                bail!("You need to pass `--substrate`, `--polkadot`, `--cumulus` or `--beefy` for `--git`.");
            } else {
                Rewrite::All
            }
        } else if self.substrate {
            Rewrite::Substrate(self.git)
        } else if self.beefy {
            Rewrite::Beefy(self.git)
        } else if self.polkadot {
            Rewrite::Polkadot(self.git)
        } else if self.cumulus {
            Rewrite::Cumulus(self.git)
        } else {
            bail!("You must specify one of `--substrate`, `--polkadot`, `--cumulus`, `--beefy` or `--all`.");
        };

        Ok((rewrite, key, self.path))
    }

    /// Run this subcommand.
    pub fn run(self) -> Result<()> {
        let (rewrite, key, path) = self.into_parts()?;

        let path = path
            .map(Ok)
            .unwrap_or_else(|| current_dir().with_context(|| "Working directory is invalid."))?;
        ensure!(
            path.is_dir(),
            "Path '{}' is not a directory.",
            path.display()
        );

        let is_hidden = |entry: &DirEntry| {
            entry
                .file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
        };

        WalkDir::new(path)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file() && e.file_name().to_string_lossy().ends_with("Cargo.toml")
            })
            .try_for_each(|toml| handle_toml_file(toml.into_path(), &rewrite, &key))
    }
}

/// Handle a given dependency.
///
/// This directly modifies the given `dep` in the requested way.
fn handle_dependency(name: &str, dep: &mut InlineTable, rewrite: &Rewrite, key: &Key) -> Result<()> {
    // `git` source
    if is_git_source(key) {
        dep.remove("tag");
        dep.remove("branch");
        dep.remove("rev");

        let git = if let Some(git) = dep
            .get("git")
            .and_then(|v| v.as_str())
            .and_then(|d| GitUrl::parse(d).ok())
        {
            git
        } else {
            return Ok(());
        };

        let new_git = match rewrite {
            Rewrite::All => &None,
            Rewrite::Substrate(new_git) if git.name == "substrate" => new_git,
            Rewrite::Polkadot(new_git) if git.name == "polkadot" => new_git,
            Rewrite::Cumulus(new_git) if git.name == "cumulus" => new_git,
            Rewrite::Beefy(new_git) if git.name == "grandpa-bridge-gadget" => new_git,
            _ => return Ok(()),
        };

        if let Some(new_git) = new_git {
            *dep.get_or_insert("git", "") = Value::from(new_git.as_str()).decorated(" ", "");
        }
    // `version` source
    } else {
        dep.remove("version");
        dep.remove("path");
    };

    match key {
        Key::Tag(tag) => {
            *dep.get_or_insert("tag", "") = Value::from(tag.as_str()).decorated(" ", " ");
        }
        Key::Branch(branch) => {
            *dep.get_or_insert("branch", "") = Value::from(branch.as_str()).decorated(" ", " ");
        }
        Key::Rev(rev) => {
            *dep.get_or_insert("rev", "") = Value::from(rev.as_str()).decorated(" ", " ");
        }
        Key::Version(source) => {
            // *dep.get_or_insert("version", "") = Value::from(ver.as_str()).decorated(" ", " ");
            let package = if let Some(package_name) = dep.get("package").and_then(|v| v.as_str()) {
                package_name
            } else {
                name
            };

            let version = get_package_version(package, source)?;

            *dep.get_or_insert("version", "") = Value::from(version.as_str()).decorated(" ", " ");
        }
    }
    log::debug!("  updated: {:?} <= {}", key, name);
    Ok(())
}

/// Handle a given `Cargo.toml`.
///
/// This means scanning all dependencies and rewrite the requested onces.
fn handle_toml_file(path: PathBuf, rewrite: &Rewrite, key: &Key) -> Result<()> {
    log::info!("Processing: {}", path.display());

    let mut toml_doc = Document::from_str(&fs::read_to_string(&path)?)?;

    // Iterate over all tables in the document
    toml_doc
        .clone()
        .iter()
        // filter out everything that is not a dependency table
        .filter(|(k, _)| k.contains("dependencies"))
        .filter_map(|(k, v)| v.as_table().map(|t| (k, t)))
        .for_each(|(k, t)| {
            t.iter()
                // Filter everything that is not an inline table (`{ foo = bar }`)
                .filter_map(|v| v.1.as_inline_table().map(|_| v.0))
                .for_each(|dn| {
                    // Get the actual inline table from the document that we modify
                    let table = toml_doc[k][dn]
                        .as_inline_table_mut()
                        .expect("We filter by `is_inline_table`; qed");
                    let _ = handle_dependency(dn, table, rewrite, key).map_err(|err| {
                        log::error!("Error handling dependency: {}", err);
                    });
                })
        });

    fs::write(&path, toml_doc.to_string())?;
    Ok(())
}

fn get_version_source(version: &String) -> Result<VersionSource> {
    let source = if version.starts_with("http://") || version.starts_with("https://") {
        VersionSource::Url(version.clone())
    } else {
        let path = PathBuf::from(version);
        if path.exists() && path.file_name() == Some("Cargo.lock".as_ref()) {
            VersionSource::File(version.clone())
        } else if version == "latest" {
            VersionSource::CratesIO
        } else {
            bail!("Invalid 'version' source: {}", version)
        }
    };
    Ok(source)
}

fn get_package_version(package: &str, source: &VersionSource) -> Result<String> {
    let version = match source {
        VersionSource::CratesIO => {
            let url = format!("https://crates.io/api/v1/crates/{}", package);
            let client = reqwest::blocking::Client::new();

            let body = client.get(&url)
                .header(USER_AGENT, "diener_crawler")
                .send()?.text()?;

            log::debug!("Crates IO plain response: {}", body);

            let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
                anyhow!("error trying to JSON parse the crates.io response: {}", body)
            })?;

            log::debug!("Crates IO json response: {}", json);

            json["crate"]["max_version"].as_str().ok_or(
                anyhow!("package '{}' not found on crates.io", package)
            )?.to_string()
        }
        VersionSource::Url(url) => {
            let body = reqwest::blocking::get(url)?.text()?;
            get_package_version_from_cargo_lock_file(body, package).ok_or(
                anyhow!("package '{}' not found in Cargo.lock", package)
            )?
        }
        VersionSource::File(path) => {
            let path = PathBuf::from(path);
            let body = fs::read_to_string(path)?;
            get_package_version_from_cargo_lock_file(body, package).ok_or(
                anyhow!("package '{}' not found in Cargo.lock", package)
            )?
        }
    };
    Ok(version)
}

fn get_package_version_from_cargo_lock_file(body: String, package_name: &str) -> Option<String> {
    let doc = body.parse::<Document>().ok()?;
    let package_table = doc["package"].as_array_of_tables()?;

    for package in package_table.iter() {
        if let Some(name) = package["name"].as_str() {
            if name == package_name {
                if let Some(version) = package["version"].as_str() {
                    return Some(version.to_string());
                }
            }
        }
    }
    None
}

fn is_git_source(key: &Key) -> bool {
    match key {
        Key::Tag(_) | Key::Branch(_) | Key::Rev(_) => true,
        _ => false,
    }
}
