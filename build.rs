use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `git <args>` and return trimmed stdout, or None if git is missing,
/// fails, or we're not in a repository (e.g. building from a crates.io tarball).
fn git(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ask cargo to rerun this script when `path` changes.  Only paths that exist
/// are registered: cargo treats a missing path as always-changed, which would
/// rerun the script on every build.
fn watch(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() {
    // Build identity, e.g. "v0.1.19-12-gabc1234-dirty": last release tag, commits
    // since it, short hash, and whether the tree had uncommitted changes.
    // --long keeps the form uniform on a release commit ("v0.1.19-0-g...").
    // --always falls back to the bare hash when no tag is reachable (shallow clone).
    let describe = git(&["describe", "--tags", "--match", "v*", "--long", "--dirty", "--always"])
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_DESCRIBE={}", describe);

    println!("cargo:rerun-if-changed=build.rs");

    // Rerun when the commit, branch, or tags change.  `.git/HEAD` alone only
    // changes on branch switch; a new commit updates the branch's ref file (or
    // packed-refs), and a new tag lands under refs/tags.  Worktrees keep HEAD
    // and index in their own git dir but share refs in the common dir.
    if let (Some(git_dir), Some(common_dir)) =
        (git(&["rev-parse", "--git-dir"]), git(&["rev-parse", "--git-common-dir"]))
    {
        let git_dir = PathBuf::from(git_dir);
        let common_dir = PathBuf::from(common_dir);
        watch(&git_dir.join("HEAD"));
        watch(&git_dir.join("index"));
        if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch(&common_dir.join(head_ref));
        }
        watch(&common_dir.join("packed-refs"));
        watch(&common_dir.join("refs/tags"));

        // "-dirty" depends on working-tree edits, which the git files above don't
        // see until staged; watch the tracked sources so it stays accurate.
        watch(Path::new("src"));
        watch(Path::new("Cargo.toml"));
        watch(Path::new("Cargo.lock"));
    }
}
