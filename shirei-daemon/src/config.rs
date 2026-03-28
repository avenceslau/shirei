use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CommandConfig {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandConfig {
    fn from_vec(parts: Vec<String>) -> Option<Self> {
        let mut iter = parts
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let program = iter.next()?;
        let args = iter.collect();
        Some(Self { program, args })
    }

    fn from_program(program: String) -> Option<Self> {
        Self::from_vec(vec![program])
    }
}

#[derive(Debug, Clone)]
pub struct CommandsConfig {
    pub opencode: CommandConfig,
    pub bootstrap: Option<CommandConfig>,
}

#[derive(Debug, Clone)]
pub struct ShireiConfig {
    pub commands: CommandsConfig,
}

impl Default for ShireiConfig {
    fn default() -> Self {
        Self {
            commands: CommandsConfig {
                opencode: CommandConfig {
                    program: "opencode".to_string(),
                    args: Vec::new(),
                },
                bootstrap: None,
            },
        }
    }
}

impl ShireiConfig {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join("config.json")
    }

    pub fn load(state_dir: &Path) -> Self {
        let path = Self::path(state_dir);
        let raw = match std::fs::read_to_string(&path) {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed reading config file; using defaults");
                return Self::default();
            }
        };

        let parsed = match serde_json::from_str::<RawConfig>(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed parsing config file; using defaults");
                return Self::default();
            }
        };

        let mut cfg = Self::default();
        if let Some(cmd) = parsed
            .commands
            .opencode
            .and_then(RawCommand::into_command_config)
        {
            cfg.commands.opencode = cmd;
        }
        cfg.commands.bootstrap = parsed
            .commands
            .bootstrap
            .and_then(RawCommand::into_command_config);

        cfg
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawConfig {
    #[serde(default)]
    commands: RawCommands,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawCommands {
    opencode: Option<RawCommand>,
    bootstrap: Option<RawCommand>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum RawCommand {
    Program(String),
    ProgramWithArgs(Vec<String>),
}

impl RawCommand {
    fn into_command_config(self) -> Option<CommandConfig> {
        match self {
            Self::Program(v) => CommandConfig::from_program(v),
            Self::ProgramWithArgs(v) => CommandConfig::from_vec(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ShireiConfig;
    use tempfile::TempDir;

    #[test]
    fn defaults_when_missing_file() {
        let td = TempDir::new().expect("tempdir");
        let cfg = ShireiConfig::load(td.path());
        assert_eq!(cfg.commands.opencode.program, "opencode");
        assert!(cfg.commands.opencode.args.is_empty());
    }

    #[test]
    fn loads_command_with_args() {
        let td = TempDir::new().expect("tempdir");
        let path = ShireiConfig::path(td.path());
        std::fs::write(
            &path,
            r#"{
  "commands": {
    "opencode": ["foo", "--bar"],
    "bootstrap": ["bash", "-lc", "npm install"]
  }
}"#,
        )
        .expect("write config");

        let cfg = ShireiConfig::load(td.path());
        assert_eq!(cfg.commands.opencode.program, "foo");
        assert_eq!(cfg.commands.opencode.args, vec!["--bar".to_string()]);
        let bootstrap = cfg.commands.bootstrap.expect("bootstrap config");
        assert_eq!(bootstrap.program, "bash");
        assert_eq!(
            bootstrap.args,
            vec!["-lc".to_string(), "npm install".to_string()]
        );
    }

    #[test]
    fn falls_back_on_parse_error() {
        let td = TempDir::new().expect("tempdir");
        let path = ShireiConfig::path(td.path());
        std::fs::write(&path, "{ this is not json }").expect("write config");

        let cfg = ShireiConfig::load(td.path());
        assert_eq!(cfg.commands.opencode.program, "opencode");
    }
}
