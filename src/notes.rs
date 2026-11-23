//! Local Markdown notebooks for the "notes" pane.
//!
//! Every notebook is a plain `<name>.md` file inside one directory. Nothing
//! leaves the machine; the page talks to the host through IPC and the host
//! calls into this module.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

const MAX_NAME: usize = 80;
const MAX_RESULTS: usize = 200;
const MAX_LINE: usize = 200;

pub struct Notes {
    dir: PathBuf,
}

impl Notes {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Notes { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Notebook names map to "<name>.md". Rejects empty names, "..", path
    /// separators, reserved Windows names, control chars, trailing dots or
    /// spaces and names over 80 chars. Returns the cleaned name.
    pub fn sanitize(name: &str) -> Result<String, String> {
        let mut n = name.trim();
        if n.len() > 3 && n[n.len() - 3..].eq_ignore_ascii_case(".md") {
            n = n[..n.len() - 3].trim_end();
        }
        if n.is_empty() {
            return Err("Notebook name is empty".into());
        }
        if n.chars().count() > MAX_NAME {
            return Err(format!("Notebook name is longer than {MAX_NAME} characters"));
        }
        if n == "." || n == ".." {
            return Err("Notebook name may not contain \"..\"".into());
        }
        if n.starts_with('.') {
            return Err("Notebook name may not start with a dot".into());
        }
        if let Some(c) = n
            .chars()
            .find(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        {
            return Err(if c.is_control() {
                "Notebook name contains a control character".into()
            } else {
                format!("Notebook name may not contain '{c}'")
            });
        }
        if n.ends_with('.') || n.ends_with(' ') {
            return Err("Notebook name may not end with a dot or space".into());
        }
        let stem = n.split('.').next().unwrap_or(n).trim_end().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if reserved {
            return Err(format!("\"{n}\" is a reserved name"));
        }
        Ok(n.to_string())
    }

    fn path(&self, name: &str) -> Result<(String, PathBuf), String> {
        let n = Self::sanitize(name)?;
        let p = self.dir.join(format!("{n}.md"));
        Ok((n, p))
    }

    /// `[{name, modified, size}]`, newest first (`modified` in ms since epoch).
    pub fn list(&self) -> Vec<Value> {
        let mut out: Vec<(String, u64, u64)> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let file_name = e.file_name();
                let Some(f) = file_name.to_str() else { continue };
                let Some(stem) = f.strip_suffix(".md") else { continue };
                if Self::sanitize(stem).ok().as_deref() != Some(stem) {
                    continue;
                }
                let Ok(md) = e.metadata() else { continue };
                if !md.is_file() {
                    continue;
                }
                let modified = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                out.push((stem.to_string(), modified, md.len()));
            }
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        out.into_iter()
            .map(|(name, modified, size)| json!({ "name": name, "modified": modified, "size": size }))
            .collect()
    }

    /// Contents of a notebook; a missing file reads as empty.
    pub fn read(&self, name: &str) -> Result<String, String> {
        let (_, p) = self.path(name)?;
        match fs::read(&p) {
            Ok(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("Cannot read notebook: {e}")),
        }
    }

    /// Replaces a notebook atomically (write a temp file, then rename).
    pub fn write(&self, name: &str, text: &str) -> Result<(), String> {
        let (n, p) = self.path(name)?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("Cannot create notes folder: {e}"))?;
        let tmp = self.dir.join(format!(".{n}.md.{}.tmp", std::process::id()));
        let res = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &p)
        })();
        if let Err(e) = res {
            let _ = fs::remove_file(&tmp);
            return Err(format!("Cannot save notebook: {e}"));
        }
        Ok(())
    }

    /// Appends text to a notebook, creating it if needed.
    pub fn append(&self, name: &str, text: &str) -> Result<(), String> {
        let (_, p) = self.path(name)?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("Cannot create notes folder: {e}"))?;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("Cannot append to notebook: {e}"))
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let (_, p) = self.path(name)?;
        match fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Cannot delete notebook: {e}")),
        }
    }

    /// Renames a notebook; fails if the target already exists.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let (fname, fp) = self.path(from)?;
        let (tname, tp) = self.path(to)?;
        if fname == tname {
            return Ok(());
        }
        if !fp.is_file() {
            return Err(format!("Notebook \"{fname}\" does not exist"));
        }
        // A case-only rename is fine on case-insensitive file systems.
        if tp.exists() && !fname.eq_ignore_ascii_case(&tname) {
            return Err(format!("A notebook named \"{tname}\" already exists"));
        }
        fs::rename(&fp, &tp).map_err(|e| format!("Cannot rename notebook: {e}"))
    }

    /// Case-insensitive full-text search over all notebooks:
    /// `[{name, line (1-based), text}]`, at most 200 results.
    pub fn search(&self, query: &str) -> Vec<Value> {
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        if q.is_empty() {
            return out;
        }
        for item in self.list() {
            let Some(name) = item["name"].as_str() else { continue };
            let Ok(text) = self.read(name) else { continue };
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&q) {
                    let t = line.trim();
                    let t: String = if t.chars().count() > MAX_LINE {
                        t.chars().take(MAX_LINE).collect()
                    } else {
                        t.to_string()
                    };
                    out.push(json!({ "name": name, "line": i + 1, "text": t }));
                    if out.len() >= MAX_RESULTS {
                        return out;
                    }
                }
            }
        }
        out
    }
}

/// Markdown block for a quick capture: a blockquote of the selected text,
/// then "— [title](url) · YYYY-MM-DD HH:MM" (local time), surrounded by
/// blank lines.
pub fn capture_block(text: &str, title: &str, url: &str) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M");
    let quote: String = text
        .trim()
        .lines()
        .map(|l| {
            let l = l.trim_end();
            if l.is_empty() { ">".to_string() } else { format!("> {l}") }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let title = title.trim();
    let title = if title.is_empty() { url } else { title };
    let title = title.replace('\n', " ").replace('[', "\\[").replace(']', "\\]");
    let url = url.trim().replace(' ', "%20").replace('(', "%28").replace(')', "%29");
    let source = if url.is_empty() { format!("— {title} · {now}") } else { format!("— [{title}]({url}) · {now}") };
    if quote.is_empty() {
        format!("\n{source}\n\n")
    } else {
        format!("\n{quote}\n\n{source}\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_notes(tag: &str) -> Notes {
        let d = std::env::temp_dir().join(format!(
            "brust-notes-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        Notes::new(d)
    }

    #[test]
    fn sanitize_cases() {
        assert_eq!(Notes::sanitize("  Inbox ").unwrap(), "Inbox");
        assert_eq!(Notes::sanitize("Ideas.md").unwrap(), "Ideas");
        assert_eq!(Notes::sanitize("v1.2 plans").unwrap(), "v1.2 plans");
        assert_eq!(Notes::sanitize("Reisen ä ü").unwrap(), "Reisen ä ü");
        assert_eq!(Notes::sanitize("COM10").unwrap(), "COM10");
        assert_eq!(Notes::sanitize("wait... what").unwrap(), "wait... what");
        assert_eq!(Notes::sanitize("console").unwrap(), "console");
        for bad in [
            "", "   ", "..", ".", "a/b", "a\\b", "../x", "...", "CON", "con", "Nul", "aux.txt", "COM1", "lpt9",
            "PRN", "a\u{0}b", "tab\there", "dot.", "a:b", "what?", ".hidden", ".md",
        ] {
            assert!(Notes::sanitize(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(Notes::sanitize(&"a".repeat(80)).is_ok());
        assert!(Notes::sanitize(&"a".repeat(81)).is_err());
    }

    #[test]
    fn write_read_append_roundtrip() {
        let n = tmp_notes("rw");
        assert_eq!(n.read("Inbox").unwrap(), "");
        n.write("Inbox", "# Hello\n").unwrap();
        assert_eq!(n.read("Inbox").unwrap(), "# Hello\n");
        n.write("Inbox", "replaced\n").unwrap();
        n.append("Inbox", "more\n").unwrap();
        assert_eq!(n.read("Inbox").unwrap(), "replaced\nmore\n");
        n.append("New", "x").unwrap();
        assert_eq!(n.read("New.md").unwrap(), "x");
        assert!(n.write("../evil", "x").is_err());
        // no temp files left behind
        let names: Vec<_> = fs::read_dir(n.dir()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let _ = fs::remove_dir_all(n.dir());
    }

    #[test]
    fn list_newest_first() {
        let n = tmp_notes("list");
        n.write("old", "1").unwrap();
        n.write("mid", "22").unwrap();
        n.write("new", "333").unwrap();
        fs::write(n.dir().join("ignored.txt"), "x").unwrap();
        let set = |name: &str, secs: u64| {
            let f = fs::File::options().write(true).open(n.dir().join(format!("{name}.md"))).unwrap();
            f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        };
        set("old", 1_000_000);
        set("mid", 2_000_000);
        set("new", 3_000_000);
        let l = n.list();
        let names: Vec<_> = l.iter().map(|v| v["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["new", "mid", "old"]);
        assert_eq!(l[0]["size"], 3);
        assert_eq!(l[0]["modified"], 3_000_000_000u64);
        let _ = fs::remove_dir_all(n.dir());
    }

    #[test]
    fn search_finds_lines() {
        let n = tmp_notes("search");
        n.write("a", "first line\n  Rust is FUN  \nnothing\n").unwrap();
        n.write("b", "fun times\n").unwrap();
        n.write("c", &format!("{}fun\n", "x".repeat(300))).unwrap();
        let r = n.search("fun");
        assert_eq!(r.len(), 3);
        let a = r.iter().find(|v| v["name"] == "a").unwrap();
        assert_eq!(a["line"], 2);
        assert_eq!(a["text"], "Rust is FUN");
        let c = r.iter().find(|v| v["name"] == "c").unwrap();
        assert_eq!(c["text"].as_str().unwrap().chars().count(), 200);
        assert!(n.search("   ").is_empty());
        assert!(n.search("absent").is_empty());
        n.write("many", &"hit\n".repeat(500)).unwrap();
        assert_eq!(n.search("hit").len(), 200);
        let _ = fs::remove_dir_all(n.dir());
    }

    #[test]
    fn rename_and_delete() {
        let n = tmp_notes("ren");
        n.write("a", "A").unwrap();
        n.write("b", "B").unwrap();
        assert!(n.rename("a", "b").is_err());
        assert!(n.rename("missing", "z").is_err());
        assert!(n.rename("a", "../z").is_err());
        n.rename("a", "c").unwrap();
        assert_eq!(n.read("c").unwrap(), "A");
        assert_eq!(n.read("a").unwrap(), "");
        n.delete("c").unwrap();
        n.delete("c").unwrap();
        let names: Vec<_> = n.list().iter().map(|v| v["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["b"]);
        let _ = fs::remove_dir_all(n.dir());
    }

    #[test]
    fn capture_block_format() {
        let b = capture_block("  hello\nworld\n\nend ", "Page [1]", "https://example.com/a b");
        let lines: Vec<&str> = b.split('\n').collect();
        assert_eq!(lines[0], "");
        assert_eq!(&lines[1..5], ["> hello", "> world", ">", "> end"]);
        assert_eq!(lines[5], "");
        let src = lines[6];
        assert!(src.starts_with("— [Page \\[1\\]](https://example.com/a%20b) · "), "{src}");
        let ts = src.rsplit(" · ").next().unwrap();
        assert!(chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M").is_ok(), "{ts}");
        assert!(b.ends_with("\n\n"));
        let no_title = capture_block("q", "", "https://x.org");
        assert!(no_title.contains("— [https://x.org](https://x.org) · "));
    }
}
