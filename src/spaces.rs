//! Spaces: named working directories, each with its own set of terminals.
//! Only the definitions are saved (`spaces.json`); the terminals themselves live in sessions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Space {
    pub id: u64,
    pub name: String,
    pub cwd: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Spaces {
    next_id: u64,
    pub spaces: Vec<Space>,
    #[serde(skip)]
    path: PathBuf,
}

impl Spaces {
    pub fn load() -> Result<Self> {
        Self::load_from(crate::store::Store::config_dir().join("spaces.json"))
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        let mut s = if path.exists() {
            let text = std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
            serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))?
        } else {
            Spaces::default()
        };
        s.path = path;
        Ok(s)
    }

    /// Atomic write: temp file + rename.
    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(()); // not backed by a file (tests)
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == id)
    }

    pub fn add(&mut self, name: String, cwd: String) -> u64 {
        self.next_id += 1;
        self.spaces.push(Space { id: self.next_id, name, cwd });
        self.next_id
    }

    pub fn rename(&mut self, id: u64, name: String) {
        if let Some(s) = self.spaces.iter_mut().find(|s| s.id == id) {
            s.name = name;
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.spaces.retain(|s| s.id != id);
    }

    /// Moves a space up or down in the list.
    pub fn shift(&mut self, id: u64, delta: i32) -> bool {
        let Some(i) = self.spaces.iter().position(|s| s.id == id) else { return false };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= self.spaces.len() {
            return false;
        }
        self.spaces.swap(i, j as usize);
        true
    }
}

/// Default name for a space: the last component of its directory.
pub fn default_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "space".into())
}

/// Current git branch of the repository containing `dir` (a short hash if HEAD is detached).
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return branch_from_head(&dot_git.join("HEAD"));
        }
        if dot_git.is_file() {
            // A worktree or submodule: `.git` is a file saying `gitdir: <path>`.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(target).is_absolute() { PathBuf::from(target) } else { d.join(target) };
            return branch_from_head(&gitdir.join("HEAD"));
        }
        cur = d.parent();
    }
    None
}

fn branch_from_head(head: &Path) -> Option<String> {
    let text = std::fs::read_to_string(head).ok()?;
    let text = text.trim();
    match text.strip_prefix("ref: ") {
        Some(r) => Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string()),
        None => Some(text.chars().take(7).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ship-spaces-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn saves_and_loads_spaces() {
        let dir = tmp("save");
        let mut s = Spaces::load_from(dir.join("spaces.json")).unwrap();
        let a = s.add("app".into(), "/work/app".into());
        let b = s.add("api".into(), "/work/api".into());
        assert!(s.shift(b, -1));
        s.rename(a, "web".into());
        s.save().unwrap();
        let t = Spaces::load_from(dir.join("spaces.json")).unwrap();
        assert_eq!(t.spaces.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["api", "web"]);
        assert_eq!(t.get(a).unwrap().cwd, "/work/app");
        let mut t = t;
        assert_eq!(t.add("x".into(), "/x".into()), 3, "ids keep growing");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_the_branch_from_a_repo_and_its_subdirectories() {
        let dir = tmp("git");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feat/week1\n").unwrap();
        assert_eq!(git_branch(&dir).as_deref(), Some("feat/week1"));
        assert_eq!(git_branch(&dir.join("src/deep")).as_deref(), Some("feat/week1"));
        std::fs::write(dir.join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(git_branch(&dir).as_deref(), Some("0123456"), "detached HEAD");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn follows_a_worktree_gitdir_file() {
        let dir = tmp("wt");
        let real = dir.join("real-gitdir");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(git_branch(&work).as_deref(), Some("topic"));
        assert_eq!(git_branch(&std::env::temp_dir().join("no-such-dir-ship")), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn names_a_space_after_its_directory() {
        assert_eq!(default_name(Path::new("/work/my-app")), "my-app");
        assert_eq!(default_name(Path::new("/")), "space");
    }
}
