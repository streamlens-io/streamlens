//! What to open: a local file or a URL, and the TLS settings every network
//! request gets.

use std::ffi::{c_char, c_int, CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::ffi::ffmpeg::*;
use crate::Error;

/// A local file, or a URL libavformat reads: an HTTP(S) file or an HLS
/// playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Url(String),
}

impl Source {
    /// A URL when `s` starts with a scheme (`https://…`), else a file path.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        let is_url = s.split_once("://").is_some_and(|(scheme, _)| {
            scheme.len() > 1
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        });
        if is_url {
            Source::Url(s.to_string())
        } else {
            Source::File(PathBuf::from(s))
        }
    }

    /// Short name for titles: the file name, or the URL's last path segment.
    pub fn name(&self) -> String {
        match self {
            Source::File(p) => p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            Source::Url(u) => {
                let path = u.split(['?', '#']).next().unwrap_or(u);
                path.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(u)
                    .to_string()
            }
        }
    }

    pub(crate) fn to_cstring(&self) -> Result<CString, Error> {
        match self {
            Source::File(p) => path_to_cstring(p),
            Source::Url(u) => CString::new(u.as_str()).map_err(|_| Error::InvalidPath),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::File(p) => write!(f, "{}", p.display()),
            Source::Url(u) => f.write_str(u),
        }
    }
}

impl From<PathBuf> for Source {
    fn from(p: PathBuf) -> Self {
        Source::File(p)
    }
}

impl From<&Path> for Source {
    fn from(p: &Path) -> Self {
        Source::File(p.to_path_buf())
    }
}

impl From<&PathBuf> for Source {
    fn from(p: &PathBuf) -> Self {
        Source::File(p.clone())
    }
}

#[cfg(unix)]
fn path_to_cstring(path: &Path) -> Result<CString, Error> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::InvalidPath)
}

// FFmpeg's file protocol takes UTF-8 paths on Windows.
#[cfg(not(unix))]
fn path_to_cstring(path: &Path) -> Result<CString, Error> {
    let s = path.to_str().ok_or(Error::InvalidPath)?;
    CString::new(s).map_err(|_| Error::InvalidPath)
}

type IoOpen = unsafe extern "C" fn(
    *mut AVFormatContext,
    *mut *mut AVIOContext,
    *const c_char,
    c_int,
    *mut *mut AVDictionary,
) -> c_int;

/// libavformat's own `io_open`, which [`open_verified`] wraps.
static DEFAULT_IO_OPEN: OnceLock<IoOpen> = OnceLock::new();

/// Routes every open of `fmt` — the input itself and nested ones like HLS
/// playlists, segments and keys — through [`open_verified`].
///
/// # Safety
/// `fmt` must come from `avformat_alloc_context`.
pub(crate) unsafe fn verify_tls(fmt: *mut AVFormatContext) {
    if let Some(default) = (*fmt).io_open {
        DEFAULT_IO_OPEN.get_or_init(|| default);
        (*fmt).io_open = Some(open_verified);
    }
}

/// Turns on certificate verification for network opens. libavformat 62
/// leaves `tls_verify` off by default, and nested opens (HLS segments)
/// would not inherit it from the input's options.
unsafe extern "C" fn open_verified(
    s: *mut AVFormatContext,
    pb: *mut *mut AVIOContext,
    url: *const c_char,
    flags: c_int,
    options: *mut *mut AVDictionary,
) -> c_int {
    let default = *DEFAULT_IO_OPEN
        .get()
        .expect("set before io_open is installed");
    let is_network = !url.is_null() && {
        let u = CStr::from_ptr(url).to_bytes();
        u.starts_with(b"https:") || u.starts_with(b"http:") || u.starts_with(b"tls:")
    };
    if !is_network {
        return default(s, pb, url, flags, options);
    }
    let mut local: *mut AVDictionary = std::ptr::null_mut();
    let opts = if options.is_null() {
        &mut local
    } else {
        options
    };
    av_dict_set(opts, c"tls_verify".as_ptr(), c"1".as_ptr(), 0);
    if let Some(ca) = ca_bundle() {
        av_dict_set(
            opts,
            c"ca_file".as_ptr(),
            ca.as_ptr(),
            AV_DICT_DONT_OVERWRITE as c_int,
        );
    }
    let ret = default(s, pb, url, flags, opts);
    av_dict_free(&mut local);
    ret
}

/// The system CA bundle for mbedTLS, which has no default trust store.
/// macOS and Windows verify against the OS store.
fn ca_bundle() -> Option<&'static CStr> {
    static BUNDLE: OnceLock<Option<CString>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            if !cfg!(target_os = "linux") {
                return None;
            }
            [
                "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu, Arch
                "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL
                "/etc/ssl/ca-bundle.pem",             // openSUSE
                "/etc/ssl/cert.pem",                  // Alpine
            ]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .and_then(|p| CString::new(p).ok())
        })
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls_and_paths() {
        let hls = "https://example.com/a/b/playlist.m3u8?token=1";
        assert_eq!(Source::parse(hls), Source::Url(hls.into()));
        assert_eq!(Source::parse(hls).name(), "playlist.m3u8");
        assert_eq!(
            Source::parse("/videos/game.mp4"),
            Source::File("/videos/game.mp4".into())
        );
        assert_eq!(Source::parse("/videos/game.mp4").name(), "game.mp4");
        assert!(matches!(
            Source::parse("C:/videos/game.mp4"),
            Source::File(_)
        ));
        assert_eq!(
            Source::parse("https://example.com/item-0001").name(),
            "item-0001"
        );
    }
}
