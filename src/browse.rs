//! `pacdeb packages <app|repository>`: everything an apt repository offers, by the
//! repository's name or by an app that comes from it.

use crate::error::{Result, bail};
use crate::paths::Paths;
use crate::registry::{Config, SourceConfig};

/// The saved repository `name` refers to: a repository, or an app with an apt source.
pub fn repository_of(config: &Config, name: &str) -> Result<String> {
    if config.apt.contains_key(name) {
        return Ok(name.to_string());
    }
    match config.apps.get(name).map(|a| &a.source) {
        Some(SourceConfig::Apt { repository, .. }) => Ok(repository.clone()),
        Some(_) => bail!("{name} does not come from an apt repository"),
        None => bail!("no app or apt repository named {name}; see 'pacdeb list' and 'pacdeb apt list'"),
    }
}

pub fn run(name: &str) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let repo = repository_of(&config, name)?;
    crate::aptrepos::run(&["packages".to_string(), repo])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{App, AptRepoConfig};

    #[test]
    fn finds_the_repository_by_app_or_name() {
        let mut config = Config::default();
        config.apt.insert("vendor".into(), AptRepoConfig { url: "https://x".into(), suite: "stable".into(), components: vec!["main".into()], arch: None, key: None, key_url: None, key_fingerprint: None });
        config.apps.insert("tool".into(), App::new(SourceConfig::Apt { repository: "vendor".into(), package: None }));
        config.apps.insert("web".into(), App::new(SourceConfig::Manual {}));
        assert_eq!(repository_of(&config, "vendor").unwrap(), "vendor");
        assert_eq!(repository_of(&config, "tool").unwrap(), "vendor");
        assert!(repository_of(&config, "web").unwrap_err().to_string().contains("does not come from"));
        assert!(repository_of(&config, "nope").is_err());
    }
}
