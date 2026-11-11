//! The dist: the clients a server hands out (docs/update.md), and the routes that hand them out,
//! `GET /api/dist/manifest.json`, `/api/dist/manifest.sig` and `/api/dist/<name>`.
//!
//! The server only serves files; it never signs or checks anything. The clients check the
//! signature of the manifest and the bytes of every file (`crate::update`).

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use http_body::Frame;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, ReadBuf};

use crate::App;
use crate::auth::Caller;
use crate::error::ApiError;

pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.sig";

/// What `manifest.json` says: the version of the clients, and each file with its size and hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub files: Vec<DistFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistFile {
    pub name: String,
    /// `binary`, `pi` or `hermes`.
    pub kind: String,
    /// The target triple of a binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Manifest> {
        let manifest: Manifest = serde_json::from_slice(bytes)
            .map_err(|e| anyhow::anyhow!("the manifest is not what a release writes: {e}"))?;
        if let Some(bad) = manifest.files.iter().find(|f| !valid_name(&f.name)) {
            anyhow::bail!("the manifest lists a file with a bad name: {:?}", bad.name);
        }
        Ok(manifest)
    }

    pub fn file(&self, name: &str) -> Option<&DistFile> {
        self.files.iter().find(|f| f.name == name)
    }
}

/// A plain file name: no directory, nothing hidden, no `.` or `..`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

/// The dist next to a database: `<its directory>/dist`.
pub fn dir_for(db_file: &str) -> PathBuf {
    match Path::new(db_file).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("dist"),
        _ => PathBuf::from("dist"),
    }
}

/// What `serve` says at start about the dist in `dir`: `Ok` with an informative line, or `Err`
/// with a warning (a dist of another version, or one that cannot be read).
pub fn start_note(dir: &Path, own_version: &str) -> Result<String, String> {
    let bytes = match std::fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(format!(
                "no dist in {}: clients are not updated from this server",
                dir.display()
            ));
        }
        Err(e) => return Err(format!("the dist in {} cannot be read: {e}", dir.display())),
    };
    let manifest =
        Manifest::parse(&bytes).map_err(|e| format!("the dist in {}: {e:#}", dir.display()))?;
    if !dir.join(SIGNATURE).exists() {
        return Err(format!(
            "the dist in {} has no {SIGNATURE}: clients refuse it",
            dir.display()
        ));
    }
    if manifest.version != own_version {
        return Err(format!(
            "the dist in {} is version {}, not this server's {own_version}: clients update to {}",
            dir.display(),
            manifest.version,
            manifest.version
        ));
    }
    Ok(format!(
        "handing out the clients of version {} from {}",
        manifest.version,
        dir.display()
    ))
}

/// `GET /api/dist/{name}`: the manifest, its signature, or a file the manifest lists, as is.
pub async fn file(
    State(app): State<Arc<App>>,
    _caller: Caller,
    name: Result<UrlPath<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let not_found = || ApiError::not_found("no such file in this server's dist");
    let UrlPath(name) = name.map_err(|_| not_found())?;
    let dir = app.dist.as_ref().ok_or_else(not_found)?;
    if !valid_name(&name) {
        return Err(not_found());
    }
    if name != MANIFEST && name != SIGNATURE {
        // Only what the manifest lists: nothing else that may sit in the directory.
        let manifest = match tokio::fs::read(dir.join(MANIFEST)).await {
            Ok(bytes) => Manifest::parse(&bytes).map_err(|_| not_found())?,
            Err(_) => return Err(not_found()),
        };
        if manifest.file(&name).is_none() {
            return Err(not_found());
        }
    }
    let file = match tokio::fs::File::open(dir.join(&name)).await {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
        Err(e) => {
            tracing::error!(error = %e, file = %name, "reading the dist");
            return Err(ApiError::internal());
        }
    };
    let meta = file.metadata().await.map_err(|_| ApiError::internal())?;
    if !meta.is_file() {
        return Err(not_found());
    }
    let kind = if name == MANIFEST {
        "application/json"
    } else {
        "application/octet-stream"
    };
    Ok((
        [
            (CONTENT_TYPE, kind.to_string()),
            (CONTENT_LENGTH, meta.len().to_string()),
            (CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::new(FileBody {
            file,
            left: meta.len(),
        }),
    )
        .into_response())
}

/// A file sent in chunks, so that a binary of several megabytes is never held whole in memory.
struct FileBody {
    file: tokio::fs::File,
    /// Bytes still to send: no more than the length announced, even if the file grows.
    left: u64,
}

const CHUNK: usize = 64 * 1024;

impl http_body::Body for FileBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.left == 0 {
            return Poll::Ready(None);
        }
        let want = CHUNK.min(self.left as usize);
        let mut buf = vec![0u8; want];
        let mut read = ReadBuf::new(&mut buf);
        match Pin::new(&mut self.file).poll_read(cx, &mut read) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let n = read.filled().len();
                if n == 0 {
                    // Shorter than announced: the file shrank under us.
                    return Poll::Ready(Some(Err(io::ErrorKind::UnexpectedEof.into())));
                }
                buf.truncate(n);
                self.left -= n as u64;
                Poll::Ready(Some(Ok(Frame::data(Bytes::from(buf)))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.left == 0
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(self.left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_plain_files() {
        for ok in [
            "manifest.json",
            "relevo-x86_64-unknown-linux-gnu",
            "relevo-pi-0.4.0.tar.gz",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "../relevo.db",
            "a\\b",
            "a\0b",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_dist_is_next_to_the_database() {
        assert_eq!(
            dir_for("/home/x/.local/share/relevo/relevo.db"),
            Path::new("/home/x/.local/share/relevo/dist")
        );
        assert_eq!(dir_for("relevo.db"), Path::new("dist"));
        assert_eq!(dir_for("./relevo.db"), Path::new("./dist"));
    }
}
