//! Fix previews, held for `fix_apply` (ADR 0006, decision 3).
//!
//! A preview is single use, lives in this process's memory only, and expires
//! ten minutes after it is made. A restarted server knows none.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a preview stays valid.
pub const PREVIEW_TTL: Duration = Duration::from_secs(10 * 60);

/// One file a preview would rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewFile {
    /// As `fix` was given it (relative to the server's working directory
    /// when inside it).
    pub path: PathBuf,
    /// SHA-256 of the file's bytes when the preview was made.
    pub old_sha256: String,
    /// SHA-256 of the source `fix` planned for it.
    pub new_sha256: String,
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub created: Instant,
    /// The paths `fix` was planned for, to plan again at apply time.
    pub paths: Vec<PathBuf>,
    pub files: Vec<PreviewFile>,
    /// The contract fingerprint the plan was made under.
    pub fingerprint: String,
}

/// Why a preview id can't be applied.
#[derive(Debug, PartialEq, Eq)]
pub enum Unusable {
    /// Never made, already used, or made by a server process that has since
    /// restarted - this one can't tell which.
    Unknown,
    Expired,
}

pub struct PreviewStore {
    ttl: Duration,
    items: HashMap<String, Preview>,
    made: u64,
}

impl PreviewStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            items: HashMap::new(),
            made: 0,
        }
    }

    /// Keep `preview` and return its id. Expired previews are dropped first.
    pub fn insert(&mut self, preview: Preview) -> String {
        let ttl = self.ttl;
        self.items.retain(|_, p| p.created.elapsed() <= ttl);
        self.made += 1;
        let id = new_id(self.made);
        self.items.insert(id.clone(), preview);
        id
    }

    /// Take the preview out of the store. It is gone after this whatever
    /// happens next, so an id is used at most once.
    pub fn take(&mut self, id: &str) -> Result<Preview, Unusable> {
        let preview = self.items.remove(id).ok_or(Unusable::Unknown)?;
        if preview.created.elapsed() > self.ttl {
            return Err(Unusable::Expired);
        }
        Ok(preview)
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// An id no earlier preview had, and not guessable from the last one.
fn new_id(n: u64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let digest = sha256_hex(format!("{}:{n}:{nanos}", std::process::id()).as_bytes());
    format!("kvp_{}", &digest[..24])
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview() -> Preview {
        Preview {
            created: Instant::now(),
            paths: vec![PathBuf::from("a.py")],
            files: Vec::new(),
            fingerprint: "a6edcff00199c1cf".into(),
        }
    }

    #[test]
    fn a_preview_is_used_once() {
        let mut store = PreviewStore::new(PREVIEW_TTL);
        let id = store.insert(preview());
        assert!(id.starts_with("kvp_"));
        assert!(store.take(&id).is_ok());
        assert_eq!(store.take(&id).unwrap_err(), Unusable::Unknown);
        assert_eq!(store.take("kvp_never").unwrap_err(), Unusable::Unknown);
    }

    #[test]
    fn a_preview_expires() {
        let mut store = PreviewStore::new(PREVIEW_TTL);
        let mut old = preview();
        old.created = Instant::now()
            .checked_sub(PREVIEW_TTL + Duration::from_secs(1))
            .expect("the clock is far enough along");
        let id = store.insert(old);
        assert_eq!(store.take(&id).unwrap_err(), Unusable::Expired);
        // ...and is gone, like a used one.
        assert_eq!(store.take(&id).unwrap_err(), Unusable::Unknown);
    }

    #[test]
    fn ids_differ() {
        let mut store = PreviewStore::new(PREVIEW_TTL);
        let a = store.insert(preview());
        let b = store.insert(preview());
        assert_ne!(a, b);
    }
}
