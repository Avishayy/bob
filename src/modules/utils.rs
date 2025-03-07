use crate::enums::VersionType;
use crate::models::{Config, InputVersion, RepoCommit, UpstreamVersion};
use anyhow::{anyhow, Result};
use dirs::{data_local_dir, home_dir};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use reqwest::Client;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::process::Command;

pub async fn parse_version_type(client: &Client, version: &str) -> Result<InputVersion> {
    match version {
        "nightly" => Ok(InputVersion {
            tag_name: version.to_string(),
            version_type: VersionType::Standard,
        }),
        "stable" => {
            let response = client
                .get("https://api.github.com/repos/neovim/neovim/releases/latest")
                .header("user-agent", "bob")
                .header("Accept", "application/vnd.github.v3+json")
                .send()
                .await?
                .text()
                .await?;

            let latest: UpstreamVersion = serde_json::from_str(&response)?;

            Ok(InputVersion {
                tag_name: latest.tag_name,
                version_type: VersionType::Standard,
            })
        }
        _ => {
            let version_regex = Regex::new(r"^v?[0-9]+\.[0-9]+\.[0-9]+$")?;
            let hash_regex = Regex::new(r"\b[0-9a-f]{5,40}\b")?;
            if version_regex.is_match(version) {
                let mut returned_version = version.to_string();
                if !version.contains('v') {
                    returned_version.insert(0, 'v');
                }
                return Ok(InputVersion {
                    tag_name: returned_version,
                    version_type: VersionType::Standard,
                });
            } else if hash_regex.is_match(version) {
                return Ok(InputVersion {
                    tag_name: version.to_string(),
                    version_type: VersionType::Hash,
                });
            }
            Err(anyhow!("Please provide a proper version string"))
        }
    }
}

pub async fn get_downloads_folder(config: &Config) -> Result<PathBuf> {
    let path = match &config.downloads_dir {
        Some(path) => {
            if tokio::fs::metadata(path).await.is_err() {
                return Err(anyhow!("Custom directory {path} doesn't exist!"));
            }

            PathBuf::from(path)
        }
        None => {
            let mut data_dir = if cfg!(target_os = "macos") {
                let mut home_dir = match home_dir() {
                    Some(home) => home,
                    None => return Err(anyhow!("Couldn't get home directory")),
                };
                home_dir.push(".local/share");
                home_dir
            } else {
                match data_local_dir() {
                    None => return Err(anyhow!("Couldn't get local data folder")),
                    Some(value) => value,
                }
            };

            data_dir.push("bob");
            let does_folder_exist = tokio::fs::metadata(&data_dir).await.is_ok();

            if !does_folder_exist && tokio::fs::create_dir(&data_dir).await.is_err() {
                return Err(anyhow!("Couldn't create downloads directory"));
            }
            data_dir
        }
    };

    Ok(path)
}

pub async fn remove_dir(directory: &str) -> Result<()> {
    let path = Path::new(directory);
    let size = path.read_dir()?.count();
    let read_dir = path.read_dir()?;

    let pb = ProgressBar::new(size.try_into()?);
    pb.set_style(ProgressStyle::default_bar()
                    .template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({per_sec}, {eta})")
                    .progress_chars("█  "));
    pb.set_message(format!("Deleting {}", path.display()));

    let mut removed: u64 = 0;

    for entry in read_dir.flatten() {
        let path = entry.path();

        if path.is_dir() {
            if let Err(e) = fs::remove_dir_all(&path).await {
                return Err(anyhow!("Failed to remove {}: {}", path.display(), e));
            }
        } else if let Err(e) = fs::remove_file(&path).await {
            return Err(anyhow!("Failed to remove {}: {}", path.display(), e));
        }
        removed += 1;
        pb.set_position(removed);
    }

    if let Err(e) = fs::remove_dir(directory).await {
        return Err(anyhow!("Failed to remove {directory}: {}", e));
    }

    pb.finish_with_message(format!("Finished removing {}", path.display()));

    Ok(())
}

pub fn get_installation_folder(config: &Config) -> Result<PathBuf> {
    match &config.installation_location {
        Some(path) => Ok(PathBuf::from(path.clone())),
        None => {
            if cfg!(target_os = "macos") {
                let mut home_dir = match home_dir() {
                    Some(home) => home,
                    None => return Err(anyhow!("Couldn't get home directory")),
                };
                home_dir.push(".local/share/neovim");
                return Ok(home_dir);
            }
            let mut data_dir = match data_local_dir() {
                None => return Err(anyhow!("Couldn't get local data folder")),
                Some(value) => value,
            };
            data_dir.push("neovim");
            Ok(data_dir)
        }
    }
}

pub fn get_file_type() -> &'static str {
    if cfg!(target_family = "windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

pub async fn is_version_installed(version: &str, config: &Config) -> Result<bool> {
    let downloads_dir = get_downloads_folder(config).await?;
    let mut dir = tokio::fs::read_dir(&downloads_dir).await?;

    while let Some(directory) = dir.next_entry().await? {
        let name = directory.file_name().to_str().unwrap().to_owned();
        if !version.contains(&name) {
            continue;
        } else {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn is_version_used(version: &str, config: &Config) -> bool {
    match get_current_version(config).await {
        Ok(value) => value.contains(version),
        Err(_) => false,
    }
}

pub async fn get_current_version(config: &Config) -> Result<String> {
    let mut downloads_dir = get_downloads_folder(config).await?;
    downloads_dir.push("used");
    match fs::read_to_string(&downloads_dir).await {
        Ok(value) => Ok(value),
        Err(error) => match error.kind() { // If used file doesn't exist try directly via neovim
            std::io::ErrorKind::NotFound => {
   let output = match Command::new("nvim").arg("--version").output().await {
        Ok(value) => value,
        Err(_) => return Err(anyhow!("Neovim is not installed")),
    };
    let output = String::from_utf8_lossy(&output.stdout).to_string();
    if output.contains("dev") {
        return Ok(String::from("nightly"));
    }
    let regex = Regex::new(r"v[0-9]\.[0-9]\.[0-9]")?;
    Ok(regex.find(output.as_str()).unwrap().as_str().to_owned())
            },
            _ => Err(anyhow!("{} is corrupted, try running bob use again or open an issue at https://github.com/MordechaiHadad/bob", downloads_dir.display())),
        },
    }
}

pub fn get_platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "nvim-win64"
    } else if cfg!(target_os = "macos") {
        "nvim-macos"
    } else {
        "nvim-linux64"
    }
}

pub async fn get_upstream_nightly(client: &Client) -> Result<UpstreamVersion> {
    let response = client
        .get("https://api.github.com/repos/neovim/neovim/releases/tags/nightly")
        .header("user-agent", "bob")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await?
        .text()
        .await?;
    match serde_json::from_str(&response) {
        Ok(value) => Ok(value),
        Err(_) => Err(anyhow!(
            "Failed to get upstream nightly version, aborting..."
        )),
    }
}

pub async fn get_local_nightly(config: &Config) -> Result<UpstreamVersion> {
    let downloads_dir = get_downloads_folder(config).await?;
    if let Ok(file) =
        fs::read_to_string(format!("{}/nightly/bob.json", downloads_dir.display())).await
    {
        let file_json: UpstreamVersion = serde_json::from_str(&file)?;
        Ok(file_json)
    } else {
        Err(anyhow!("Couldn't find bob.json"))
    }
}

pub async fn get_commits_for_nightly(
    client: &Client,
    since: &str,
    until: &str,
) -> Result<Vec<RepoCommit>> {
    let response = client
        .get(format!(
            "https://api.github.com/repos/neovim/neovim/commits?since={since}&until={until}&per_page=100"))
        .header("user-agent", "bob")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await?
        .text()
        .await?;

    Ok(serde_json::from_str(&response)?)
}

pub async fn handle_subprocess(process: &mut Command) -> Result<()> {
    match process.status().await?.code() {
        Some(0) => Ok(()),
        Some(code) => Err(anyhow!(code)),
        None => Err(anyhow!("process terminated by signal")),
    }
}
