use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use chrono::Utc;
use tauri::State;
use tokio::process::Command;
use uuid::Uuid;

use crate::commands::python_runtime::{
    build_python_path, enrich_python_error, resolve_python_executable, RuntimePaths,
};
use crate::dto::{
    NotebookCellDto, NotebookCellExecutionDto, NotebookDocumentDto, NotebookRunRequestDto,
    NotebookSummaryDto,
};

const DEFAULT_NOTEBOOK_TITLE: &str = "Research Notebook";

#[tauri::command]
pub async fn list_notebooks(
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<Vec<NotebookSummaryDto>, String> {
    let root = notebooks_root(&runtime_paths)?;
    let mut entries = Vec::new();

    for entry in fs::read_dir(&root).map_err(|error| {
        format!(
            "Failed to read notebooks directory {}: {error}",
            root.display()
        )
    })? {
        let entry = entry.map_err(|error| format!("Failed to read notebook entry: {error}"))?;
        let path = entry.path();
        if !path.is_file() || !is_notebook_path(&path) {
            continue;
        }

        let metadata = entry.metadata().map_err(|error| {
            format!(
                "Failed to read notebook metadata for {}: {error}",
                path.display()
            )
        })?;
        let updated_at = metadata
            .modified()
            .ok()
            .map(chrono::DateTime::<Utc>::from)
            .unwrap_or_else(Utc::now)
            .to_rfc3339();

        entries.push(NotebookSummaryDto {
            name: notebook_display_name(&path),
            path: path
                .strip_prefix(runtime_paths.work_root())
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string(),
            updated_at,
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

#[tauri::command]
pub async fn load_notebook(
    path: String,
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<NotebookDocumentDto, String> {
    let notebook_path = resolve_notebook_path(&runtime_paths, &path)?;
    read_notebook(&notebook_path, runtime_paths.work_root())
}

#[tauri::command]
pub async fn create_notebook(
    path: String,
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<NotebookDocumentDto, String> {
    let notebook_path = resolve_notebook_path(&runtime_paths, &path)?;

    if notebook_path.exists() {
        return read_notebook(&notebook_path, runtime_paths.work_root());
    }

    let title = notebook_display_name(&notebook_path);
    let notebook = NotebookDocumentDto {
        title,
        path: notebook_path
            .strip_prefix(runtime_paths.work_root())
            .unwrap_or(&notebook_path)
            .to_string_lossy()
            .to_string(),
        cells: vec![
            NotebookCellDto {
                id: format!("cell-{}", Uuid::new_v4()),
                kind: "markdown".into(),
                content:
                    "# Research notes\n\nCapture hypotheses, observations, and next steps here."
                        .into(),
                output: None,
            },
            NotebookCellDto {
                id: format!("cell-{}", Uuid::new_v4()),
                kind: "code".into(),
                content: "import math\nprint('Notebook ready', math.pi)".into(),
                output: None,
            },
        ],
        updated_at: Utc::now().to_rfc3339(),
    };

    save_notebook_to_path(&notebook_path, &notebook, runtime_paths.work_root())?;
    read_notebook(&notebook_path, runtime_paths.work_root())
}

#[tauri::command]
pub async fn save_notebook(
    notebook: NotebookDocumentDto,
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<NotebookDocumentDto, String> {
    let notebook_path = resolve_notebook_path(&runtime_paths, &notebook.path)?;
    save_notebook_to_path(&notebook_path, &notebook, runtime_paths.work_root())?;
    read_notebook(&notebook_path, runtime_paths.work_root())
}

#[tauri::command]
pub async fn run_notebook_cell(
    request: NotebookRunRequestDto,
    runtime_paths: State<'_, RuntimePaths>,
) -> Result<NotebookCellExecutionDto, String> {
    let _notebook_path = resolve_notebook_path(&runtime_paths, &request.path)?;
    let target_index = request
        .cells
        .iter()
        .position(|cell| cell.id == request.cell_id)
        .ok_or_else(|| format!("Notebook cell `{}` was not found", request.cell_id))?;
    let target_cell = &request.cells[target_index];

    if target_cell.kind != "code" {
        return Err("Only code cells can be executed in the research notebook".to_string());
    }

    let script = request
        .cells
        .iter()
        .take(target_index + 1)
        .filter(|cell| cell.kind == "code")
        .map(|cell| cell.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    if script.trim().is_empty() {
        return Err("Notebook code cell is empty".to_string());
    }

    let python = resolve_python_executable(
        &runtime_paths,
        "APEX_NOTEBOOK_PYTHON",
        &["APEX_STRATEGY_PYTHON", "APEX_PYTHON_PATH"],
    )?;
    let python_path = build_python_path(&runtime_paths)?;

    let output = Command::new(&python)
        .arg("-c")
        .arg(script)
        .current_dir(runtime_paths.work_root())
        .env("PYTHONPATH", python_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| {
            format!(
                "Failed to execute notebook cell with {}: {error}",
                python.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let success = output.status.success();
    let stderr = if success {
        stderr
    } else {
        enrich_python_error(
            &stderr,
            &[
                "APEX_NOTEBOOK_PYTHON",
                "APEX_STRATEGY_PYTHON",
                "APEX_PYTHON_PATH",
            ],
        )
    };

    Ok(NotebookCellExecutionDto {
        cell_id: request.cell_id,
        success,
        stdout,
        stderr,
        finished_at: Utc::now().to_rfc3339(),
    })
}

fn notebooks_root(runtime_paths: &RuntimePaths) -> Result<PathBuf, String> {
    let root = runtime_paths.resolve_user_relative_path("notebooks");
    fs::create_dir_all(&root).map_err(|error| {
        format!(
            "Failed to create notebooks directory {}: {error}",
            root.display()
        )
    })?;
    Ok(root)
}

fn resolve_notebook_path(
    runtime_paths: &RuntimePaths,
    requested_path: &str,
) -> Result<PathBuf, String> {
    let root = notebooks_root(runtime_paths)?;
    let raw = requested_path.trim();
    let relative = if raw.is_empty() {
        PathBuf::from("research.apexnb.json")
    } else {
        PathBuf::from(raw)
    };

    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(
            "Notebook paths must stay within the notebooks workspace directory".to_string(),
        );
    }

    // Listing, creating and saving return paths relative to the work root
    // ("notebooks/research.apexnb.json"), while callers may also pass a path relative to the
    // notebooks directory ("research.apexnb.json"). Accept both instead of nesting
    // "notebooks/notebooks/...".
    let relative = match relative.strip_prefix("notebooks") {
        Ok(rest) if !rest.as_os_str().is_empty() => rest.to_path_buf(),
        _ => relative,
    };

    let with_extension = if is_notebook_path(&relative) {
        relative
    } else {
        let mut normalized = relative;
        normalized.set_extension("apexnb.json");
        normalized
    };

    let resolved = root.join(with_extension);
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create notebook directory {}: {error}",
                parent.display()
            )
        })?;
    }
    Ok(resolved)
}

fn read_notebook(path: &Path, work_root: &Path) -> Result<NotebookDocumentDto, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read notebook {}: {error}", path.display()))?;
    let mut notebook: NotebookDocumentDto = serde_json::from_str(&raw)
        .map_err(|error| format!("Failed to parse notebook {}: {error}", path.display()))?;
    notebook.path = path
        .strip_prefix(work_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Ok(notebook)
}

fn save_notebook_to_path(
    path: &Path,
    notebook: &NotebookDocumentDto,
    work_root: &Path,
) -> Result<(), String> {
    let mut normalized = notebook.clone();
    normalized.title = if normalized.title.trim().is_empty() {
        DEFAULT_NOTEBOOK_TITLE.to_string()
    } else {
        normalized.title.trim().to_string()
    };
    normalized.path = path
        .strip_prefix(work_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    normalized.updated_at = Utc::now().to_rfc3339();

    let json = serde_json::to_string_pretty(&normalized)
        .map_err(|error| format!("Failed to serialize notebook {}: {error}", path.display()))?;
    fs::write(path, json)
        .map_err(|error| format!("Failed to save notebook {}: {error}", path.display()))
}

fn is_notebook_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.ends_with(".apexnb.json"))
        .unwrap_or(false)
}

fn notebook_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(".apexnb.json").replace('_', " "))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_NOTEBOOK_TITLE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("apex-notebook-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn work_root_relative_and_notebooks_relative_paths_resolve_to_the_same_file() {
        let root = temp_root("paths");
        let paths = RuntimePaths::for_test(&root);
        let expected = root.join("notebooks").join("research.apexnb.json");

        for input in [
            "notebooks/research.apexnb.json",
            "research.apexnb.json",
            "research",
            "",
        ] {
            let resolved = resolve_notebook_path(&paths, input).unwrap();
            assert_eq!(resolved, expected, "input {input:?}");
        }
        assert!(!root.join("notebooks").join("notebooks").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn subfolders_and_traversal_are_handled() {
        let root = temp_root("sub");
        let paths = RuntimePaths::for_test(&root);

        let nested = resolve_notebook_path(&paths, "notebooks/ideas/a.apexnb.json").unwrap();
        assert_eq!(nested, root.join("notebooks").join("ideas").join("a.apexnb.json"));
        assert!(resolve_notebook_path(&paths, "../escape.apexnb.json").is_err());
        assert!(resolve_notebook_path(&paths, "notebooks/../escape.apexnb.json").is_err());
        assert!(resolve_notebook_path(&paths, "/etc/x.apexnb.json").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_path_returned_by_read_loads_back() {
        let root = temp_root("roundtrip");
        let paths = RuntimePaths::for_test(&root);
        let file = resolve_notebook_path(&paths, "research.apexnb.json").unwrap();
        let doc = NotebookDocumentDto {
            title: "research".into(),
            path: String::new(),
            cells: vec![],
            updated_at: Utc::now().to_rfc3339(),
        };
        save_notebook_to_path(&file, &doc, paths.work_root()).unwrap();

        let listed = read_notebook(&file, paths.work_root()).unwrap().path;
        assert_eq!(listed, "notebooks/research.apexnb.json");
        let again = resolve_notebook_path(&paths, &listed).unwrap();
        assert!(read_notebook(&again, paths.work_root()).is_ok());
        let _ = fs::remove_dir_all(&root);
    }
}
