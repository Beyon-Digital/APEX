use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Runtime};

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    repo_root: PathBuf,
    work_root: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    python_root: PathBuf,
    data_dir: PathBuf,
    models_dir: PathBuf,
    strategies_dir: PathBuf,
    resource_dir: Option<PathBuf>,
    bundled: bool,
}

impl RuntimePaths {
    pub fn resolve<R: Runtime>(app: &AppHandle<R>) -> Result<Self, String> {
        let repo_root = repo_root()?;
        let resource_dir = app.path().resource_dir().ok();
        let python_root = resolve_python_root(resource_dir.as_deref(), &repo_root)?;
        let bundled = python_root != repo_root.join("apex-python");

        let work_root = if bundled {
            let app_data_dir = app
                .path()
                .app_local_data_dir()
                .or_else(|_| app.path().app_data_dir())
                .map_err(|error| format!("Failed to resolve app data directory: {error}"))?;
            ensure_dir_exists(&app_data_dir)?
        } else {
            repo_root.clone()
        };

        let config_dir = if bundled {
            let app_config_dir = app
                .path()
                .app_config_dir()
                .map_err(|error| format!("Failed to resolve app config directory: {error}"))?;
            ensure_dir_exists(&app_config_dir)?
        } else {
            ensure_dir_exists(&repo_root.join("config"))?
        };
        let config_file = config_dir.join("apex.toml");

        let data_dir = ensure_dir_exists(&work_root.join("data"))?;
        let models_dir = ensure_dir_exists(&work_root.join("models"))?;
        let strategies_dir = ensure_dir_exists(&work_root.join("strategies"))?;

        Ok(Self {
            repo_root,
            work_root,
            config_dir,
            config_file,
            python_root,
            data_dir,
            models_dir,
            strategies_dir,
            resource_dir,
            bundled,
        })
    }

    /// Paths rooted in one directory, for tests that need storage without a Tauri app.
    #[cfg(test)]
    pub fn for_test(root: &Path) -> Self {
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).expect("create test data dir");
        Self {
            repo_root: root.to_path_buf(),
            work_root: root.to_path_buf(),
            config_dir: root.join("config"),
            config_file: root.join("config").join("apex.toml"),
            python_root: root.join("apex-python"),
            data_dir,
            models_dir: root.join("models"),
            strategies_dir: root.join("strategies"),
            resource_dir: None,
            bundled: false,
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn work_root(&self) -> &Path {
        &self.work_root
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn python_root(&self) -> &Path {
        &self.python_root
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub fn strategies_dir(&self) -> &Path {
        &self.strategies_dir
    }

    pub fn resource_dir(&self) -> Option<&Path> {
        self.resource_dir.as_deref()
    }

    pub fn is_bundled(&self) -> bool {
        self.bundled
    }

    pub fn resolve_user_relative_path<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.work_root.join(path)
        }
    }

    /// Inverse of [`resolve_user_relative_path`]: the path as the trainer and strategy files
    /// should name it (relative to `work_root`, e.g. `data/exports/X.csv`).
    pub fn to_user_relative_path<P: AsRef<Path>>(&self, path: P) -> String {
        let path = path.as_ref();
        path.strip_prefix(&self.work_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}

pub fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Unable to determine repository root".to_string())
}

pub fn build_python_path(runtime_paths: &RuntimePaths) -> Result<OsString, String> {
    let mut paths = vec![runtime_paths.python_root().to_path_buf()];

    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        paths.extend(std::env::split_paths(&existing));
    }

    std::env::join_paths(paths).map_err(|e| format!("Failed to build PYTHONPATH: {e}"))
}

pub fn resolve_python_executable(
    runtime_paths: &RuntimePaths,
    primary_env: &str,
    fallback_envs: &[&str],
) -> Result<PathBuf, String> {
    let configured = std::iter::once(primary_env)
        .chain(fallback_envs.iter().copied())
        .map(|env_name| (env_name, std::env::var(env_name).ok()))
        .collect::<Vec<_>>();
    let borrowed = configured
        .iter()
        .map(|(env_name, value)| (*env_name, value.as_deref()))
        .collect::<Vec<_>>();

    resolve_python_executable_for_root(runtime_paths.repo_root(), &borrowed)
}

pub fn enrich_python_error(message: &str, env_vars: &[&str]) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.contains("ModuleNotFoundError") || trimmed.contains("No module named") {
        let env_list = env_vars.join(" or ");
        return format!(
            "{trimmed}. Install the `apex-python` dependencies and point {env_list} at the project virtualenv interpreter if needed."
        );
    }

    trimmed.to_string()
}

fn resolve_python_executable_for_root(
    repo_root: &Path,
    configured: &[(&str, Option<&str>)],
) -> Result<PathBuf, String> {
    let candidates = python_executable_candidates(repo_root);

    for (env_name, value) in configured {
        let Some(raw) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };

        if is_generic_python_command(raw) {
            if let Some(found) = first_existing_path(&candidates) {
                return Ok(found);
            }
            return Ok(PathBuf::from(raw));
        }

        let command = resolve_command_path(repo_root, raw);
        if looks_like_path(raw) && !command.exists() {
            return Err(format!(
                "Configured {env_name} does not exist: {}",
                command.display()
            ));
        }

        return Ok(command);
    }

    if let Some(found) = first_existing_path(&candidates) {
        return Ok(found);
    }

    Ok(PathBuf::from(default_python_command()))
}

fn python_executable_candidates(repo_root: &Path) -> Vec<PathBuf> {
    let python_root = repo_root.join("apex-python");
    [
        python_root.join(".venv"),
        repo_root.join(".venv"),
        python_root.join("venv"),
        repo_root.join("venv"),
    ]
    .into_iter()
    .map(|venv_root| venv_python_path(&venv_root))
    .collect()
}

fn venv_python_path(venv_root: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_root.join("Scripts").join("python.exe")
    } else {
        venv_root.join("bin").join("python")
    }
}

fn first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
}

fn resolve_command_path(repo_root: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else if looks_like_path(raw) {
        repo_root.join(path)
    } else {
        path
    }
}

fn looks_like_path(raw: &str) -> bool {
    raw.starts_with('.') || raw.contains('/') || raw.contains('\\')
}

fn is_generic_python_command(raw: &str) -> bool {
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "python" | "python3" | "python.exe" | "python3.exe"
    )
}

fn default_python_command() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn resolve_python_root(resource_dir: Option<&Path>, repo_root: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();

    if let Some(resource_dir) = resource_dir {
        candidates.push(resource_dir.join("apex-python"));
        candidates.push(resource_dir.join("resources").join("apex-python"));
    }

    candidates.push(repo_root.join("apex-python"));

    candidates
        .into_iter()
        .find(|candidate| candidate.is_dir())
        .ok_or_else(|| {
            let resource_hint = resource_dir
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "<none>".to_string());
            format!(
                "Unable to locate the bundled or local apex-python directory (resource_dir={resource_hint}, repo_root={}).",
                repo_root.display()
            )
        })
}

fn ensure_dir_exists(path: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "Failed to create runtime directory {}: {error}",
            path.display()
        )
    })?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "apex-python-runtime-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn prefers_local_virtualenv_for_generic_python_placeholder() {
        let root = test_root("generic-placeholder");
        let expected = root.join("apex-python").join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        });
        fs::create_dir_all(expected.parent().expect("virtualenv parent"))
            .expect("create virtualenv directory");
        fs::write(&expected, "#!/usr/bin/env python\n").expect("write fake interpreter");

        let resolved = resolve_python_executable_for_root(
            &root,
            &[("APEX_TEST_PYTHON_PATH", Some("python3"))],
        )
        .expect("resolve interpreter");

        assert_eq!(resolved, expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn normalizes_repo_relative_interpreter_paths() {
        let root = test_root("relative-path");
        let expected = root.join("apex-python").join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        });
        fs::create_dir_all(expected.parent().expect("interpreter parent"))
            .expect("create interpreter directory");
        fs::write(&expected, "#!/usr/bin/env python\n").expect("write fake interpreter");

        let resolved = resolve_python_executable_for_root(
            &root,
            &[(
                "APEX_TEST_PYTHON_PATH",
                Some("apex-python/.venv/bin/python"),
            )],
        )
        .expect("resolve interpreter");

        assert_eq!(resolved, expected);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_missing_configured_interpreter_paths() {
        let root = test_root("missing-path");
        fs::create_dir_all(&root).expect("create root directory");

        let error = resolve_python_executable_for_root(
            &root,
            &[(
                "APEX_TEST_PYTHON_PATH",
                Some("apex-python/.venv/bin/python"),
            )],
        )
        .expect_err("expected missing interpreter error");

        assert!(error.contains("APEX_TEST_PYTHON_PATH"));
        assert!(error.contains("apex-python/.venv/bin/python"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exported_paths_resolve_back_to_the_same_file() {
        let root = test_root("user-relative");
        let paths = RuntimePaths::for_test(&root);
        let file = paths.data_dir().join("exports").join("AAPL_1d.csv");
        let rel = paths.to_user_relative_path(&file);
        assert_eq!(rel, "data/exports/AAPL_1d.csv");
        assert_eq!(paths.resolve_user_relative_path(&rel), file);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn falls_back_to_default_python_when_no_env_or_virtualenv_exists() {
        let root = test_root("default-python");
        fs::create_dir_all(&root).expect("create root directory");

        let resolved =
            resolve_python_executable_for_root(&root, &[]).expect("resolve fallback interpreter");

        assert_eq!(resolved, PathBuf::from(default_python_command()));

        let _ = fs::remove_dir_all(root);
    }
}
