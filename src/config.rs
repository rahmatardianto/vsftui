use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

fn default_protocol() -> String {
    "sftp".to_string()
}

fn default_port() -> u16 {
    22
}

fn default_remote_path() -> String {
    "/".to_string()
}

fn default_context() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_debounce_ms() -> u64 {
    300
}

fn default_ignore() -> Vec<String> {
    vec![
        "**/.vscode/**".to_string(),
        "**/.git/**".to_string(),
        "**/.DS_Store".to_string(),
        "**/node_modules/**".to_string(),
        "**/target/**".to_string(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HopConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WatcherConfig {
    #[serde(default)]
    pub files: Option<String>,
    #[serde(default)]
    pub auto_upload: Option<bool>,
    #[serde(default)]
    pub auto_delete: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncOption {
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub skip_create: bool,
    #[serde(default)]
    pub ignore_existing: bool,
    #[serde(default)]
    pub update: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitAutoSyncConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub include_untracked: bool,
    #[serde(default = "default_true")]
    pub include_staged: bool,
    #[serde(default = "default_true")]
    pub include_modified: bool,
    #[serde(default)]
    pub propagate_deletes: bool,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
}

impl Default for GitAutoSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_untracked: true,
            include_staged: true,
            include_modified: true,
            propagate_deletes: false,
            debounce_ms: 300,
        }
    }
}

/// SftpConfigOverride represents profile-level overrides in `.vscode/sftp.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SftpConfigOverride {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub interactive_auth: Option<bool>,
    #[serde(default)]
    pub hop: Option<HopConfig>,
    #[serde(default)]
    pub remote_path: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub upload_on_save: Option<bool>,
    #[serde(default)]
    pub use_temp_file: Option<bool>,
    #[serde(default)]
    pub open_ssh: Option<bool>,
    #[serde(default)]
    pub download_on_open: Option<bool>,
    #[serde(default)]
    pub sync_option: Option<SyncOption>,
    #[serde(default, deserialize_with = "deserialize_octal_perm")]
    pub file_perm: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_octal_perm")]
    pub dir_perm: Option<u32>,
    #[serde(default)]
    pub ignore: Option<Vec<String>>,
    #[serde(default)]
    pub watcher: Option<WatcherConfig>,
    #[serde(default)]
    pub git_auto_sync: Option<GitAutoSyncConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpConfig {
    #[serde(default)]
    pub name: Option<String>,
    pub host: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub interactive_auth: Option<bool>,
    #[serde(default)]
    pub hop: Option<HopConfig>,
    #[serde(default = "default_remote_path")]
    pub remote_path: String,
    #[serde(default = "default_context")]
    pub context: String,
    #[serde(default)]
    pub upload_on_save: Option<bool>,
    #[serde(default)]
    pub use_temp_file: Option<bool>,
    #[serde(default)]
    pub open_ssh: Option<bool>,
    #[serde(default)]
    pub download_on_open: Option<bool>,
    #[serde(default)]
    pub sync_option: Option<SyncOption>,
    #[serde(default, deserialize_with = "deserialize_octal_perm")]
    pub file_perm: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_octal_perm")]
    pub dir_perm: Option<u32>,
    #[serde(default = "default_ignore")]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub watcher: Option<WatcherConfig>,
    #[serde(default)]
    pub profiles: Option<HashMap<String, SftpConfigOverride>>,
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub git_auto_sync: Option<GitAutoSyncConfig>,

    #[serde(skip)]
    pub(crate) ignore_glob_set: Option<GlobSet>,
}

impl SftpConfig {
    /// Expand paths (~, $ENV_VAR) and compile ignore globs.
    pub fn finalize(&mut self) -> Result<()> {
        if let Some(ref path) = self.private_key_path {
            self.private_key_path = Some(expand_env_and_tilde(path));
        }
        if let Some(ref agent) = self.agent {
            self.agent = Some(expand_env_and_tilde(agent));
        }
        self.context = expand_env_and_tilde(&self.context);
        self.remote_path = expand_env_and_tilde(&self.remote_path);

        if let Some(ref mut hop) = self.hop {
            if let Some(ref path) = hop.private_key_path {
                hop.private_key_path = Some(expand_env_and_tilde(path));
            }
            if let Some(ref agent) = hop.agent {
                hop.agent = Some(expand_env_and_tilde(agent));
            }
        }

        // Build GlobSet for ignore patterns (supports vscode-sftp / gitignore-style globs)
        let mut builder = GlobSetBuilder::new();
        for pattern in &self.ignore {
            let p = pattern.replace('\\', "/");
            let p = p.trim_start_matches("./");
            if p.is_empty() {
                continue;
            }
            if let Some(anchored) = p.strip_prefix('/') {
                add_glob_and_dir(&mut builder, anchored);
            } else if !p.contains('/') {
                add_glob_and_dir(&mut builder, p);
                add_glob_and_dir(&mut builder, &format!("**/{}", p));
            } else {
                add_glob_and_dir(&mut builder, p);
            }
        }
        match builder.build() {
            Ok(set) => self.ignore_glob_set = Some(set),
            Err(e) => {
                tracing::warn!("Failed to build ignore glob set: {}", e);
                self.ignore_glob_set = None;
            }
        }

        Ok(())
    }

    /// Check if a relative path matches the ignore rules.
    pub fn is_ignored(&self, relative_path: &str) -> bool {
        let normalized = relative_path.replace('\\', "/");
        let trimmed = normalized.trim_start_matches('/');

        if let Some(ref set) = self.ignore_glob_set {
            if set.is_match(trimmed) {
                return true;
            }
        }

        // Default safeguards
        trimmed.starts_with(".git/") || trimmed == ".git" || trimmed.starts_with(".vscode/")
    }

    /// Merge profile override into this config.
    pub fn apply_override(&self, profile_name: &str, o: &SftpConfigOverride) -> Self {
        let mut merged = self.clone();
        merged.name = Some(o.name.clone().unwrap_or_else(|| profile_name.to_string()));
        if let Some(ref host) = o.host {
            merged.host = host.clone();
        }
        if let Some(ref proto) = o.protocol {
            merged.protocol = proto.clone();
        }
        if let Some(port) = o.port {
            merged.port = port;
        }
        if let Some(ref username) = o.username {
            merged.username = username.clone();
        }
        if let Some(ref password) = o.password {
            merged.password = Some(password.clone());
        }
        if let Some(ref key) = o.private_key_path {
            merged.private_key_path = Some(key.clone());
        }
        if let Some(ref pass) = o.passphrase {
            merged.passphrase = Some(pass.clone());
        }
        if let Some(ref agent) = o.agent {
            merged.agent = Some(agent.clone());
        }
        if let Some(interactive) = o.interactive_auth {
            merged.interactive_auth = Some(interactive);
        }
        if let Some(ref hop) = o.hop {
            merged.hop = Some(hop.clone());
        }
        if let Some(ref remote) = o.remote_path {
            merged.remote_path = remote.clone();
        }
        if let Some(ref ctx) = o.context {
            merged.context = ctx.clone();
        }
        if let Some(uos) = o.upload_on_save {
            merged.upload_on_save = Some(uos);
        }
        if let Some(utf) = o.use_temp_file {
            merged.use_temp_file = Some(utf);
        }
        if let Some(ossh) = o.open_ssh {
            merged.open_ssh = Some(ossh);
        }
        if let Some(doo) = o.download_on_open {
            merged.download_on_open = Some(doo);
        }
        if let Some(ref sync_opt) = o.sync_option {
            merged.sync_option = Some(sync_opt.clone());
        }
        if let Some(fp) = o.file_perm {
            merged.file_perm = Some(fp);
        }
        if let Some(dp) = o.dir_perm {
            merged.dir_perm = Some(dp);
        }
        if let Some(ref ign) = o.ignore {
            merged.ignore = ign.clone();
        }
        if let Some(ref watcher) = o.watcher {
            merged.watcher = Some(watcher.clone());
        }
        if let Some(ref gas) = o.git_auto_sync {
            merged.git_auto_sync = Some(gas.clone());
        }

        let _ = merged.finalize();
        merged
    }
}

pub fn expand_env_and_tilde(raw: &str) -> String {
    match shellexpand::full(raw) {
        Ok(expanded) => expanded.to_string(),
        Err(_) => raw.to_string(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawConfigFile {
    Single(SftpConfig),
    Multiple(Vec<SftpConfig>),
}

/// Holds all loaded configurations and profiles.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ConfigStore {
    pub default_profile: String,
    pub profiles: HashMap<String, SftpConfig>,
    pub workspace_dir: PathBuf,
}

impl ConfigStore {
    /// Load from a given path (e.g. `.vscode/sftp.json`).
    pub fn load(config_path: &Path) -> Result<Self> {
        let mut file = File::open(config_path)
            .with_context(|| format!("Failed to open config file: {}", config_path.display()))?;

        let mut content = String::new();
        file.read_to_string(&mut content)?;

        // Try JSON5 first (which natively supports comments and trailing commas)
        let raw: RawConfigFile = match json5::from_str(&content) {
            Ok(r) => r,
            Err(_) => {
                // Fallback to stripping comments and parsing standard JSON
                let mut stripped = String::new();
                json_comments::StripComments::new(content.as_bytes()).read_to_string(&mut stripped)?;
                serde_json::from_str(&stripped)
                    .with_context(|| format!("Failed to parse JSON/JSONC in: {}", config_path.display()))?
            }
        };

        let workspace_dir = config_path
            .parent()
            .and_then(|p| {
                if p.ends_with(".vscode") {
                    p.parent()
                } else {
                    Some(p)
                }
            })
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut profiles = HashMap::new();
        let mut default_profile = String::new();

        match raw {
            RawConfigFile::Single(mut base) => {
                let _ = base.finalize();
                let base_name = base
                    .name
                    .clone()
                    .unwrap_or_else(|| "default".to_string());
                default_profile = base
                    .default_profile
                    .clone()
                    .unwrap_or_else(|| base_name.clone());

                // Insert base config first so explicit profiles map entries take precedence on name collision
                profiles.insert(base_name, base.clone());

                if let Some(ref prof_map) = base.profiles {
                    for (p_name, p_override) in prof_map {
                        let merged = base.apply_override(p_name, p_override);
                        profiles.insert(p_name.clone(), merged);
                    }
                }
            }
            RawConfigFile::Multiple(list) => {
                for (idx, mut cfg) in list.into_iter().enumerate() {
                    let _ = cfg.finalize();
                    let name = cfg
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("profile-{}", idx + 1));
                    if default_profile.is_empty() {
                        default_profile = name.clone();
                    }
                    profiles.insert(name, cfg);
                }
            }
        }

        if profiles.is_empty() {
            return Err(anyhow!("No valid profiles found in config file"));
        }

        // If default_profile wasn't set or not in map, pick first available
        if !profiles.contains_key(&default_profile) {
            default_profile = profiles.keys().next().cloned().unwrap();
        }

        Ok(Self {
            default_profile,
            profiles,
            workspace_dir,
        })
    }

    /// Locate default `.vscode/sftp.json` in workspace or parents.
    pub fn discover_and_load(workspace_root: &Path, custom_path: Option<&Path>) -> Result<Self> {
        if let Some(custom) = custom_path {
            return Self::load(custom);
        }

        let candidates = [
            workspace_root.join(".vscode").join("sftp.json"),
            workspace_root.join(".vscode").join("sftp.jsonc"),
            workspace_root.join("sftp.json"),
        ];

        for path in &candidates {
            if path.exists() {
                return Self::load(path);
            }
        }

        Err(anyhow!(
            "No sftp.json found. Looked in: {:?}",
            candidates
        ))
    }

    pub fn get_profile(&self, name: Option<&str>) -> Option<&SftpConfig> {
        let key = name.unwrap_or(&self.default_profile);
        self.profiles.get(key)
    }

    pub fn profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.profiles.keys().cloned().collect();
        names.sort();
        names
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::io::Write;

    #[test]
    fn test_parse_jsonc_with_comments_and_profiles() {
        let jsonc_content = r#"
        // VS Code SFTP Configuration
        {
            "name": "Default Dev",
            "host": "dev.example.com",
            "protocol": "sftp",
            "port": 2222,
            "username": "deploy",
            "privateKeyPath": "~/.ssh/id_ed25519",
            "remotePath": "/var/www/dev",
            "uploadOnSave": true,
            "useTempFile": true,
            "ignore": [
                "**/.git/**",
                "**/node_modules/**"
            ],
            /* Extended Git Auto-Sync block */
            "gitAutoSync": {
                "enabled": true,
                "includeUntracked": true,
                "includeStaged": true,
                "includeModified": true,
                "propagateDeletes": false,
                "debounceMs": 250
            },
            "profiles": {
                "staging": {
                    "host": "staging.example.com",
                    "remotePath": "/var/www/staging",
                },
                "production": {
                    "host": "prod.example.com",
                    "remotePath": "/var/www/prod",
                    "useTempFile": false
                }
            }
        }
        "#;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("sftp.json");
        let mut file = File::create(&config_file).unwrap();
        file.write_all(jsonc_content.as_bytes()).unwrap();

        let store = ConfigStore::load(&config_file).expect("Failed to load JSONC config");
        assert_eq!(store.profile_names().len(), 3);
        assert!(store.profile_names().contains(&"Default Dev".to_string()));
        assert!(store.profile_names().contains(&"staging".to_string()));
        assert!(store.profile_names().contains(&"production".to_string()));

        // Check Default profile
        let def = store.get_profile(Some("Default Dev")).unwrap();
        assert_eq!(def.host, "dev.example.com");
        assert_eq!(def.port, 2222);
        assert_eq!(def.remote_path, "/var/www/dev");
        assert_eq!(def.use_temp_file, Some(true));
        let gas = def.git_auto_sync.as_ref().unwrap();
        assert!(gas.enabled);
        assert_eq!(gas.debounce_ms, 250);

        // Check staging override
        let staging = store.get_profile(Some("staging")).unwrap();
        assert_eq!(staging.host, "staging.example.com");
        assert_eq!(staging.port, 2222); // Inherited
        assert_eq!(staging.remote_path, "/var/www/staging");

        // Check production override
        let prod = store.get_profile(Some("production")).unwrap();
        assert_eq!(prod.host, "prod.example.com");
        assert_eq!(prod.use_temp_file, Some(false));

        // Check ignore pattern matching
        assert!(def.is_ignored(".git/config"));
        assert!(def.is_ignored("node_modules/package.json"));
        assert!(!def.is_ignored("src/main.rs"));
    }

    #[test]
    fn test_parse_multi_config_array() {
        let array_content = r#"
        [
            {
                "name": "Server A",
                "host": "server-a.com",
                "username": "user1"
            },
            {
                "name": "Server B",
                "host": "server-b.com",
                "username": "user2"
            }
        ]
        "#;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("sftp.json");
        let mut file = File::create(&config_file).unwrap();
        file.write_all(array_content.as_bytes()).unwrap();

        let store = ConfigStore::load(&config_file).expect("Failed to load multi config array");
        assert_eq!(store.profile_names().len(), 2);
        assert!(store.get_profile(Some("Server A")).is_some());
        assert!(store.get_profile(Some("Server B")).is_some());
    }

    #[test]
    fn test_vscode_sftp_compat_ignore_and_default_profile() {
        let json_content = r#"{
            "name": "test",
            "host": "example",
            "protocol": "sftp",
            "port": 22,
            "username": "test",
            "password": "password",
            "remotePath": "htdocs/test/",
            "uploadOnSave": false,
            "syncOption": { "delete": true },
            "ignore": [
                ".vscode",
                ".DS_Store",
                "/public/data",
                "readme.md",
                "/vendor",
                "composer.json",
                "cron/logs/**/*"
            ],
            "profiles": {
                "test": { "host": "example", "username": "test", "remotePath": "htdocs/test/" },
                "prod": { "host": "example", "username": "prod-user", "remotePath": "htdocs/prod/" }
            },
            "defaultProfile": "prod"
        }"#;

        let dir = tempdir().unwrap();
        let config_file = dir.path().join("sftp.json");
        std::fs::write(&config_file, json_content).unwrap();

        let store = ConfigStore::load(&config_file).unwrap();
        assert_eq!(store.default_profile, "prod");
        let prod = store.get_profile(None).unwrap();
        assert_eq!(prod.username, "prod-user");
        assert!(prod.sync_option.as_ref().unwrap().delete);

        assert!(prod.is_ignored(".vscode/sftp.json"));
        assert!(prod.is_ignored("vendor/autoload.php"));
        assert!(prod.is_ignored("public/data/cache.json"));
        assert!(prod.is_ignored("sub/readme.md"));
        assert!(prod.is_ignored("composer.json"));
        assert!(prod.is_ignored("cron/logs/2026/app.log"));
        assert!(!prod.is_ignored("application/controllers/Home.php"));
    }

    #[test]
    fn test_parse_file_perm_and_dir_perm() {
        let json_content = r#"{
            "host": "example",
            "username": "u",
            "filePerm": "644",
            "dirPerm": 755,
            "profiles": {
                "exec": { "filePerm": "0755" }
            }
        }"#;
        let dir = tempdir().unwrap();
        let config_file = dir.path().join("sftp.json");
        std::fs::write(&config_file, json_content).unwrap();

        let store = ConfigStore::load(&config_file).unwrap();
        let def = store.get_profile(None).unwrap();
        assert_eq!(def.file_perm, Some(0o644));
        assert_eq!(def.dir_perm, Some(0o755));
        let exec = store.get_profile(Some("exec")).unwrap();
        assert_eq!(exec.file_perm, Some(0o755));
        assert_eq!(exec.dir_perm, Some(0o755));
    }
}

fn deserialize_octal_perm<'de, D>(deserializer: D) -> std::result::Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            u32::from_str_radix(trimmed, 8)
                .map(Some)
                .map_err(|_| serde::de::Error::custom(format!("invalid octal permission: {}", s)))
        }
        Some(serde_json::Value::Number(n)) => {
            let s = n.to_string();
            u32::from_str_radix(&s, 8)
                .map(Some)
                .map_err(|_| serde::de::Error::custom(format!("invalid octal permission: {}", s)))
        }
        Some(other) => Err(serde::de::Error::custom(format!("expected octal perm string or number, got {}", other))),
    }
}

fn add_glob_and_dir(builder: &mut GlobSetBuilder, pat: &str) {
    match Glob::new(pat) {
        Ok(glob) => {
            builder.add(glob);
        }
        Err(e) => {
            tracing::warn!("Ignoring invalid ignore glob '{}': {}", pat, e);
            return;
        }
    }
    if !pat.ends_with('*') {
        let dir_pat = format!("{}/**", pat.trim_end_matches('/'));
        if let Ok(glob) = Glob::new(&dir_pat) {
            builder.add(glob);
        }
    }
}
