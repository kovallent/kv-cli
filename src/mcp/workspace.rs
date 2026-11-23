//! Where the MCP server may read and write: its roots, and how a tool's path
//! argument is checked against them (ADR 0006, decision 5).
//!
//! The roots come from the host - the server's working directory, or the
//! roots the client declares - which the user configures. A path argument
//! comes from the model, which is what a prompt injection would steer, so it
//! is the path argument that is checked, every time.

use std::path::{Component, Path, PathBuf};

/// The server's roots. Every path a tool reads or writes resolves inside one.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// The process's working directory, canonical: the default root, and what
    /// paths are shown relative to.
    cwd: PathBuf,
    /// The roots, canonical. The working directory unless the client declared
    /// roots of its own.
    roots: Vec<PathBuf>,
}

impl Workspace {
    /// A workspace rooted at the process's working directory.
    pub fn from_cwd() -> Result<Self, String> {
        let cwd = std::env::current_dir()
            .and_then(|d| d.canonicalize())
            .map_err(|e| format!("cannot resolve the working directory: {e}"))?;
        Ok(Self::at(cwd))
    }

    /// A workspace rooted at `cwd`, which must be canonical.
    pub fn at(cwd: PathBuf) -> Self {
        Self {
            roots: vec![cwd.clone()],
            cwd,
        }
    }

    /// A workspace whose root is `root`, for tests: paths are given to
    /// kv-cli absolute, since `root` isn't the process's working directory.
    #[cfg(test)]
    pub fn rooted_at(root: PathBuf) -> Self {
        let cwd = std::env::current_dir()
            .and_then(|d| d.canonicalize())
            .expect("the test's working directory resolves");
        Self {
            roots: vec![root],
            cwd,
        }
    }

    /// The same working directory, with the roots the client declared, given
    /// as `file://` URIs. A root that isn't a `file://` URI, or doesn't
    /// exist, is ignored; if none is left, no path resolves.
    pub fn with_client_roots(&self, uris: &[String]) -> Self {
        let roots = uris
            .iter()
            .filter_map(|u| file_uri_to_path(u))
            .filter_map(|p| p.canonicalize().ok())
            .filter(|p| p.is_dir())
            .collect();
        Self {
            cwd: self.cwd.clone(),
            roots,
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The root a relative path is joined to: the first.
    fn primary(&self) -> Result<&Path, String> {
        self.roots.first().map(PathBuf::as_path).ok_or_else(|| {
            "the client declared no usable roots, so no path can be read".to_string()
        })
    }

    /// Resolve a tool's path argument to the path kv-cli is given, or refuse
    /// it. `..`, absolute paths and symbolic links are resolved first, so a
    /// path that lands outside every root is refused however it is spelled.
    /// The error names the argument, never what it resolved to.
    pub fn resolve(&self, arg: &str) -> Result<PathBuf, String> {
        if arg.is_empty() {
            return Err("a path argument is empty".to_string());
        }
        if arg.contains('\0') {
            return Err(format!("path {arg:?} contains a NUL byte"));
        }
        let given = Path::new(arg);
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            self.primary()?.join(given)
        };
        let real = joined.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("path `{arg}` doesn't exist")
            } else {
                format!("path `{arg}` can't be resolved: {e}")
            }
        })?;
        if !self.contains(&real) {
            return Err(format!(
                "path `{arg}` is outside the workspace; kv-cli's MCP tools only read and \
                 write inside it"
            ));
        }
        Ok(self.shown(&real))
    }

    /// Whether `real`, a canonical path, is inside a root. Compared component
    /// by component, so `/repo-other` is not inside `/repo`.
    pub fn contains(&self, real: &Path) -> bool {
        self.roots.iter().any(|r| real.starts_with(r))
    }

    /// Re-check, at write time, that `path` still resolves inside a root.
    pub fn check_write(&self, path: &Path) -> Result<(), String> {
        let real = path
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
        if self.contains(&real) {
            Ok(())
        } else {
            Err(format!(
                "{} now resolves outside the workspace; nothing was written",
                path.display()
            ))
        }
    }

    /// The roots, as the paths kv-cli is given when a tool names none.
    pub fn default_paths(&self) -> Result<Vec<PathBuf>, String> {
        self.primary()?;
        Ok(self.roots.iter().map(|r| self.shown(r)).collect())
    }

    /// Where contract discovery starts for the contract tools.
    pub fn discovery_start(&self) -> Result<PathBuf, String> {
        Ok(self.shown(self.primary()?))
    }

    /// A canonical path as kv-cli is given it: relative to the working
    /// directory when inside it, as `kv-cli` would be run from there, so
    /// paths in results read as they do on the command line. Absolute
    /// otherwise.
    fn shown(&self, real: &Path) -> PathBuf {
        match real.strip_prefix(&self.cwd) {
            Ok(rel) if rel.as_os_str().is_empty() => PathBuf::from("."),
            Ok(rel) => rel.to_path_buf(),
            Err(_) => real.to_path_buf(),
        }
    }
}

/// `file:///home/me/repo` -> `/home/me/repo`, percent-decoded. On Windows,
/// `file:///C:/repo` -> `C:/repo`. `None` for anything that isn't a local
/// `file://` URI.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` has an empty authority; `file://localhost/path` is the
    // same place. Any other host is not on this machine.
    let path = if let Some(p) = rest.strip_prefix("localhost") {
        p
    } else if rest.starts_with('/') {
        rest
    } else {
        return None;
    };
    let decoded = percent_decode(path)?;
    let decoded = if cfg!(windows) {
        // `/C:/repo` -> `C:/repo`
        let b = decoded.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[2] == b':' {
            decoded[1..].to_string()
        } else {
            decoded
        }
    } else {
        decoded
    };
    let p = PathBuf::from(decoded);
    // A root must be absolute and must not climb.
    if !p.is_absolute() || p.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    Some(p)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kvcli-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("inside/sub")).unwrap();
        std::fs::write(d.join("inside/a.py"), "x = 1\n").unwrap();
        std::fs::write(d.join("outside.py"), "x = 1\n").unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn paths_inside_the_root_resolve_relative_to_it() {
        let d = tempdir("inside");
        let ws = Workspace {
            cwd: d.join("inside"),
            roots: vec![d.join("inside")],
        };
        assert_eq!(ws.resolve("a.py").unwrap(), PathBuf::from("a.py"));
        assert_eq!(ws.resolve(".").unwrap(), PathBuf::from("."));
        assert_eq!(ws.resolve("sub/../a.py").unwrap(), PathBuf::from("a.py"));
        let abs = d.join("inside/a.py");
        assert_eq!(
            ws.resolve(abs.to_str().unwrap()).unwrap(),
            PathBuf::from("a.py")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn escapes_are_refused_naming_the_argument() {
        let d = tempdir("escape");
        let ws = Workspace::at(d.join("inside"));
        for arg in [
            "../outside.py",
            "sub/../../outside.py",
            d.join("outside.py").to_str().unwrap(),
        ] {
            let e = ws.resolve(arg).unwrap_err();
            assert!(e.contains("outside the workspace"), "{arg}: {e}");
            assert!(e.contains(arg), "names the argument: {e}");
        }
        assert!(ws
            .resolve("/")
            .unwrap_err()
            .contains("outside the workspace"));
        assert!(ws
            .resolve("missing.py")
            .unwrap_err()
            .contains("doesn't exist"));
        assert!(ws.resolve("").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let d = tempdir("link");
        std::os::unix::fs::symlink(d.join("outside.py"), d.join("inside/link.py")).unwrap();
        std::os::unix::fs::symlink(&d, d.join("inside/up")).unwrap();
        let ws = Workspace::at(d.join("inside"));
        assert!(ws
            .resolve("link.py")
            .unwrap_err()
            .contains("outside the workspace"));
        assert!(ws
            .resolve("up")
            .unwrap_err()
            .contains("outside the workspace"));
        assert!(ws
            .resolve("up/outside.py")
            .unwrap_err()
            .contains("outside the workspace"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_outside() {
        let d = tempdir("prefix");
        std::fs::create_dir_all(d.join("inside-other")).unwrap();
        std::fs::write(d.join("inside-other/b.py"), "").unwrap();
        let ws = Workspace::at(d.join("inside"));
        assert!(ws
            .resolve(d.join("inside-other/b.py").to_str().unwrap())
            .unwrap_err()
            .contains("outside the workspace"));
        let _ = std::fs::remove_dir_all(&d);
    }

    // Unix paths make `file://` URIs directly; src/mcp/tests.rs covers roots
    // on every platform.
    #[cfg(unix)]
    #[test]
    fn client_roots_replace_the_working_directory() {
        let d = tempdir("roots");
        let ws = Workspace::at(d.clone());
        let uri = format!("file://{}", d.join("inside/sub").display());
        let narrowed = ws.with_client_roots(&[uri, "https://example.com/x".into()]);
        assert_eq!(narrowed.roots(), &[d.join("inside/sub")]);
        assert!(narrowed
            .resolve(d.join("inside/a.py").to_str().unwrap())
            .unwrap_err()
            .contains("outside the workspace"));
        let none = ws.with_client_roots(&["https://example.com/x".into()]);
        assert!(none
            .resolve("a.py")
            .unwrap_err()
            .contains("no usable roots"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn file_uris_decode() {
        if cfg!(windows) {
            return;
        }
        assert_eq!(
            file_uri_to_path("file:///a%20b/c"),
            Some(PathBuf::from("/a b/c"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/x"),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(file_uri_to_path("file://host/x"), None);
        assert_eq!(file_uri_to_path("file:///a/../b"), None);
        assert_eq!(file_uri_to_path("http:///x"), None);
        assert_eq!(file_uri_to_path("file:///bad%zz"), None);
    }
}
