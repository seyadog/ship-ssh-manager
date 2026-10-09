//! Spaces: named working directories, each with its own set of terminals.
//! Only the definitions are saved (`spaces.json`); the terminals themselves live in sessions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Space {
    pub id: u64,
    pub name: String,
    /// Where the space's terminal was left: it follows the shell's directory.
    pub cwd: String,
    /// The name follows the directory until the user renames the space.
    #[serde(default = "yes")]
    pub auto_name: bool,
    /// The folder it is filed in, if any.
    #[serde(default)]
    pub folder: Option<u64>,
    /// When something was last run in it (unix seconds). A filed project shows on top for a while after that.
    #[serde(default)]
    pub used_at: u64,
}

/// A folder of projects: it only groups them in the lower part of the list.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpaceFolder {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub expanded: bool,
}

/// How long a filed project stays on top of the list after you last ran something in it.
const PROMOTED_FOR: u64 = 3 * 24 * 3600;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn yes() -> bool {
    true
}

#[derive(Default, Serialize, Deserialize)]
pub struct Spaces {
    next_id: u64,
    pub spaces: Vec<Space>,
    #[serde(default)]
    pub folders: Vec<SpaceFolder>,
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
        self.spaces.push(Space { id: self.next_id, name, cwd, auto_name: true, folder: None, used_at: 0 });
        self.next_id
    }

    /// Moves a space to the top of the list (the most recently used first). Returns true if the order changed.
    pub fn touch(&mut self, id: u64) -> bool {
        match self.spaces.iter().position(|s| s.id == id) {
            Some(i) if i > 0 => {
                let s = self.spaces.remove(i);
                self.spaces.insert(0, s);
                true
            }
            _ => false,
        }
    }

    /// A name chosen by the user: it no longer follows the directory.
    pub fn rename(&mut self, id: u64, name: String) {
        if let Some(s) = self.spaces.iter_mut().find(|s| s.id == id) {
            s.name = name;
            s.auto_name = false;
        }
    }

    /// The terminal moved to another directory (`auto_name` is the name it would get). Returns true if
    /// anything changed.
    pub fn follow_by_dir(&mut self, id: u64, cwd: &str, auto_name: String) -> bool {
        let Some(s) = self.spaces.iter_mut().find(|s| s.id == id) else { return false };
        if s.cwd == cwd {
            return false;
        }
        s.cwd = cwd.to_string();
        if s.auto_name {
            s.name = auto_name;
        }
        true
    }

    pub fn folder(&self, id: u64) -> Option<&SpaceFolder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn add_folder(&mut self, name: String) -> u64 {
        self.next_id += 1;
        self.folders.push(SpaceFolder { id: self.next_id, name, expanded: true });
        self.next_id
    }

    /// The id of the folder with this name (ignoring case), if there is one.
    pub fn folder_named(&self, name: &str) -> Option<u64> {
        self.folders.iter().find(|f| f.name.eq_ignore_ascii_case(name)).map(|f| f.id)
    }

    /// Deletes a folder; its projects go back to the plain list.
    pub fn remove_folder(&mut self, id: u64) {
        self.folders.retain(|f| f.id != id);
        for s in self.spaces.iter_mut().filter(|s| s.folder == Some(id)) {
            s.folder = None;
        }
    }

    /// Files a project in a folder (or takes it out). It leaves the top of the list until it is used again.
    pub fn set_folder(&mut self, id: u64, folder: Option<u64>) {
        if let Some(s) = self.spaces.iter_mut().find(|s| s.id == id) {
            s.folder = folder;
            s.used_at = 0;
        }
    }

    /// Shown in the top part of the list: every project that is not filed, and the filed ones used recently.
    pub fn on_top(&self, s: &Space) -> bool {
        s.folder.is_none() || now().saturating_sub(s.used_at) < PROMOTED_FOR && s.used_at > 0
    }

    /// Something was run in this project. Returns true if that puts a filed project on top.
    pub fn mark_used(&mut self, id: u64) -> bool {
        let was = self.spaces.iter().find(|s| s.id == id).is_some_and(|s| self.on_top(s));
        if let Some(s) = self.spaces.iter_mut().find(|s| s.id == id) {
            s.used_at = now();
        }
        !was
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
        assert!(!s.follow_by_dir(a, "/work/app", "app".into()), "same directory: nothing changes");
        assert!(s.follow_by_dir(a, "/work/app/sub", "sub".into()));
        assert_eq!(s.get(a).unwrap().name, "web", "a name chosen by the user stays");
        assert!(s.follow_by_dir(b, "/work/api/v2", "v2".into()));
        assert_eq!(s.get(b).unwrap().name, "v2", "an automatic name follows the directory");
        s.save().unwrap();
        let t = Spaces::load_from(dir.join("spaces.json")).unwrap();
        assert_eq!(t.spaces.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["v2", "web"]);
        assert_eq!(t.get(a).unwrap().cwd, "/work/app/sub");
        let mut t = t;
        assert_eq!(t.add("x".into(), "/x".into()), 3, "ids keep growing");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filed_projects_leave_the_top_until_they_are_used() {
        let mut s = Spaces::default();
        let a = s.add("a".into(), "/a".into());
        let b = s.add("b".into(), "/b".into());
        let f = s.add_folder("Work".into());
        assert!(s.on_top(s.get(a).unwrap()), "an unfiled project is on top");
        s.set_folder(a, Some(f));
        assert!(!s.on_top(s.get(a).unwrap()), "filed and never used: only in its folder");
        assert_eq!(s.folder_named("work"), Some(f), "names match without regard to case");
        assert!(s.mark_used(a), "using it puts it on top");
        assert!(s.on_top(s.get(a).unwrap()));
        assert!(!s.mark_used(a), "already on top");
        s.get(b).unwrap();
        s.remove_folder(f);
        assert_eq!(s.get(a).unwrap().folder, None, "deleting a folder frees its projects");
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
