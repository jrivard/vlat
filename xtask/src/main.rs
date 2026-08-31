// xtask - repo automation, invoked as `cargo xtask <command>` (see .cargo/config.toml).
//
// Dependency-free on purpose: this crate only shells out to `trunk` and
// `git`, so `cargo xtask ...` stays cheap to compile no matter what native
// or web deps the main `vlat` package has pulled in.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GH_PAGES_BRANCH: &str = "gh-pages";

fn main() {
    let mut args = env::args().skip(1);
    let command = args.next();

    let result = match command.as_deref() {
        Some("build-web") => build_web(&repo_root()),
        Some("publish-web") => publish_web(&repo_root(), GH_PAGES_BRANCH),
        Some(other) => Err(format!("unknown command '{other}'\n\n{}", usage())),
        None => Err(usage()),
    };

    if let Err(msg) = result {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    "usage: cargo xtask <command>\n\n\
     commands:\n  \
     build-web    trunk-build the browser demo into docs/\n  \
     publish-web  build-web, then commit docs/ onto the local gh-pages branch\n"
        .to_string()
}

/// Workspace root, i.e. the repo root (xtask's manifest dir is `<repo>/xtask`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask should be a workspace member one directory below the repo root")
        .to_path_buf()
}

fn build_web(repo_root: &Path) -> Result<(), String> {
    check_trunk_installed()?;
    check_wasm_target_installed();

    println!("==> trunk build --release");
    run("trunk", &["build", "--release"], repo_root)?;

    println!("==> built docs/ at {}", repo_root.join("docs").display());
    Ok(())
}

fn check_trunk_installed() -> Result<(), String> {
    let found = Command::new("trunk")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if found {
        Ok(())
    } else {
        Err("`trunk` not found on PATH. Install it with:\n\n    cargo install trunk\n".to_string())
    }
}

/// Best-effort: only warns, since not every toolchain install is rustup-managed.
fn check_wasm_target_installed() {
    let Ok(output) = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        return; // no rustup on PATH - trust the user's toolchain has the target
    };

    let installed = String::from_utf8_lossy(&output.stdout);
    if !installed.lines().any(|line| line.trim() == "wasm32-unknown-unknown") {
        eprintln!(
            "warning: wasm32-unknown-unknown target not found in `rustup target list --installed`.\n\
             If the build below fails, run:\n\n    rustup target add wasm32-unknown-unknown\n"
        );
    }
}

fn publish_web(repo_root: &Path, branch: &str) -> Result<(), String> {
    build_web(repo_root)?;

    let docs_dir = repo_root.join("docs");
    if !docs_dir.is_dir() {
        return Err(format!("expected build output at {}", docs_dir.display()));
    }

    let source_sha = capture("git", &["rev-parse", "--short", "HEAD"], repo_root)?;
    let worktree_dir = repo_root.join("target").join("gh-pages-worktree");

    // Clean up any stale worktree from a previous interrupted run.
    if worktree_dir.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force", path_str(&worktree_dir)])
            .current_dir(repo_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if worktree_dir.exists() {
            fs::remove_dir_all(&worktree_dir)
                .map_err(|e| format!("failed to remove stale {}: {e}", worktree_dir.display()))?;
        }
    }

    let branch_exists = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    println!("==> preparing {branch} worktree at {}", worktree_dir.display());
    if branch_exists {
        run("git", &["worktree", "add", path_str(&worktree_dir), branch], repo_root)?;
    } else {
        run("git", &["worktree", "add", "--detach", path_str(&worktree_dir)], repo_root)?;
        run("git", &["checkout", "--orphan", branch], &worktree_dir)?;
        // --orphan keeps the checked-out files from HEAD in the working tree;
        // clear both the index and the working tree so only docs/ remains.
        let _ = run("git", &["rm", "-rf", "--quiet", "."], &worktree_dir);
    }

    clear_dir_except_git(&worktree_dir)?;
    copy_dir_all(&docs_dir, &worktree_dir)?;

    run("git", &["add", "-A"], &worktree_dir)?;

    let dirty = !Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(&worktree_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(true);

    if dirty {
        let message = format!("Publish web build from {source_sha}");
        run("git", &["commit", "-m", &message], &worktree_dir)?;
        println!("==> committed to {branch}");
    } else {
        println!("==> {branch} already up to date, nothing to commit");
    }

    run("git", &["worktree", "remove", "--force", path_str(&worktree_dir)], repo_root)?;

    println!(
        "\n{branch} updated locally. Review it, then push explicitly:\n\n    git push origin {branch}:{branch}\n"
    );
    Ok(())
}

fn clear_dir_except_git(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let result = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        result.map_err(|e| format!("removing {}: {e}", path.display()))?;
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in fs::read_dir(src).map_err(|e| format!("reading {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("reading {}: {e}", src.display()))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            fs::create_dir_all(&dst_path).map_err(|e| format!("creating {}: {e}", dst_path.display()))?;
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("copying {} -> {}: {e}", src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

fn run(cmd: &str, args: &[&str], cwd: &Path) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| format!("failed to run `{cmd} {}`: {e}", args.join(" ")))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("`{cmd} {}` exited with {status}", args.join(" ")))
    }
}

fn capture(cmd: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run `{cmd} {}`: {e}", args.join(" ")))?;

    if !output.status.success() {
        return Err(format!("`{cmd} {}` exited with {}", args.join(" "), output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("repo paths are expected to be valid UTF-8")
}
