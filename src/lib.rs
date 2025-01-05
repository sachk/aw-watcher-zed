use std::fs;

use serde::Deserialize;
use zed_extension_api::{
    self as zed, serde_json, settings::LspSettings, Command, LanguageServerId, Result, Worktree,
};

#[derive(Deserialize)]
struct Configuration {
    host: Option<String>,
    port: Option<u16>,
}
struct ActivityWatchExtension {
    cached_ls_binary_path: Option<String>,
}

impl ActivityWatchExtension {
    fn target_triple(&self) -> Result<String> {
        let (platform, arch) = zed::current_platform();
        let (arch, os) = {
            let arch = match arch {
                zed::Architecture::Aarch64 => "aarch64",
                zed::Architecture::X8664 => "x86_64",
                _ => return Err(format!("unsupported architecture: {arch:?}")),
            };

            let os = match platform {
                zed::Os::Mac => "apple-darwin",
                zed::Os::Linux => "unknown-linux-gnu",
                zed::Os::Windows => "pc-windows-msvc",
            };

            (arch, os)
        };

        Ok(format!("activitywatch-ls-{arch}-{os}"))
    }

    fn download(&self, language_server_id: &LanguageServerId, repo: &str) -> Result<String> {
        let release = zed::latest_github_release(
            repo,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let target_triple = self.target_triple()?;

        let asset_name = format!("{target_triple}.zip");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset found matching {:?}", asset_name))?;

        let version_dir = format!("activitywatch-ls-{}", release.version);
        let binary_path = format!("{version_dir}/activitywatch-ls");
        if !fs::metadata(&binary_path).map_or(false, |stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::Zip,
            )
            .map_err(|err| format!("failed to download file: {err}"))?;

            // Delete old versions
            let entries = fs::read_dir(".")
                .map_err(|err| format!("failed to list working directory {err}"))?;
            for entry in entries {
                let entry = entry.map_err(|err| format!("failed to load directory entry {err}"))?;
                if let Some(file_name) = entry.file_name().to_str() {
                    if file_name.starts_with("activitywatch-ls") && file_name != version_dir {
                        fs::remove_dir_all(entry.path()).ok();
                    }
                }
            }
        }

        zed::make_file_executable(&binary_path)?;

        Ok(binary_path)
    }

    fn language_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<String, String> {
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        if let Some(path) = worktree.which("activitywatch-ls") {
            return Ok(path.clone());
        }

        let target_triple = self.target_triple()?;
        if let Some(path) = worktree.which(&target_triple) {
            return Ok(path.clone());
        }

        if let Some(path) = &self.cached_ls_binary_path {
            if fs::metadata(path).map_or(false, |stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        let binary_path = self.download(language_server_id, "sachk/aw-watcher-zed")?;

        self.cached_ls_binary_path = Some(binary_path.clone());

        Ok(binary_path)
    }
}

impl zed::Extension for ActivityWatchExtension {
    fn new() -> Self {
        Self {
            cached_ls_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        // TODO: clean up below
        let lsp_settings =
            LspSettings::for_worktree(language_server_id.to_string().as_str(), worktree)?;

        let args = match lsp_settings.settings {
            Some(s) => match serde_json::from_value::<Configuration>(s) {
                Ok(config) => {
                    let mut args = Vec::new();
                    if let Some(host) = config.host {
                        args.push("--host".to_string());
                        args.push(host);
                    }
                    if let Some(port) = config.port {
                        args.push("--port".to_string());
                        args.push(port.to_string());
                    }
                    args
                }
                Err(e) => {
                    println!("error! {e:#?}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        let ls_binary_path = self.language_server_binary_path(language_server_id, worktree)?;

        Ok(Command {
            args,
            command: ls_binary_path,
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(ActivityWatchExtension);
