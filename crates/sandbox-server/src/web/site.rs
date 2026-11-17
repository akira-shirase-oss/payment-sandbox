//! A built static app held in memory (D-061). Read once at startup, bounded in files and
//! bytes, with no symbolic links followed: a request never touches the file system, so no
//! path can escape the directory. The SHA-256 of every inline script of `index.html` is
//! computed here, so the page policy can allow exactly those and no `'unsafe-inline'`; so is
//! that of every static `style="…"` attribute the app's scripts would insert (`SvelteKit`'s
//! route announcer has one), which the policy allows by hash under `'unsafe-hashes'`: only
//! those exact values, never an arbitrary inline style.

use std::collections::HashMap;
use std::path::Path;

use axum::body::Bytes;
use sha2::{Digest as _, Sha256};

/// Most files a site may have.
pub const MAX_FILES: usize = 512;
/// Most bytes all its files may hold together.
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// Deepest directory nesting read.
const MAX_DEPTH: usize = 8;

/// One file.
#[derive(Debug, Clone)]
pub struct Asset {
    /// Contents.
    pub body: Bytes,
    /// `Content-Type`.
    pub content_type: &'static str,
    /// True for content-hashed build output, cached for a year.
    pub immutable: bool,
}

/// A site.
#[derive(Debug, Clone)]
pub struct Site {
    files: HashMap<String, Asset>,
    index: Bytes,
    script_hashes: String,
    style_hashes: String,
}

/// Why a site could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum SiteError {
    /// Reading failed.
    #[error("reading the site: {0}")]
    Io(#[from] std::io::Error),
    /// No `index.html`.
    #[error("the site has no index.html (build it first: npm run build)")]
    NoIndex,
    /// Over a bound.
    #[error(
        "the site exceeds its bounds ({MAX_FILES} files, {MAX_TOTAL_BYTES} bytes, depth {MAX_DEPTH})"
    )]
    TooLarge,
    /// A symbolic link, or a name that is not plain UTF-8.
    #[error("the site contains a symbolic link or a non-UTF-8 name: {0}")]
    Unusual(String),
}

impl Site {
    /// Reads `dir` (blocking: call it from `spawn_blocking` or before serving).
    pub fn load(dir: &Path) -> Result<Self, SiteError> {
        let mut files = HashMap::new();
        let mut total = 0_u64;
        read_dir(dir, "", 0, &mut files, &mut total)?;
        let index = files
            .get("index.html")
            .ok_or(SiteError::NoIndex)?
            .body
            .clone();
        let html =
            std::str::from_utf8(&index).map_err(|_| SiteError::Unusual("index.html".to_owned()))?;
        let script_hashes = inline_script_hashes(html).join(" ");
        let mut styles = std::collections::BTreeSet::new();
        for (path, asset) in &files {
            if std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
                && let Ok(text) = std::str::from_utf8(&asset.body)
            {
                styles.extend(style_attribute_hashes(text));
            }
        }
        let style_hashes = styles.into_iter().collect::<Vec<_>>().join(" ");
        Ok(Self {
            files,
            index,
            script_hashes,
            style_hashes,
        })
    }

    /// The file at `path` (no leading slash), if any.
    pub fn get(&self, path: &str) -> Option<&Asset> {
        self.files.get(path)
    }

    /// The app's one page (the single-page app answers every route).
    pub fn index(&self) -> Bytes {
        self.index.clone()
    }

    /// The `'sha256-…'` sources of `index.html`'s inline scripts.
    pub fn script_hashes(&self) -> &str {
        &self.script_hashes
    }

    /// The `'sha256-…'` sources of the static style attributes in the app's scripts.
    pub fn style_hashes(&self) -> &str {
        &self.style_hashes
    }
}

fn read_dir(
    dir: &Path,
    prefix: &str,
    depth: usize,
    files: &mut HashMap<String, Asset>,
    total: &mut u64,
) -> Result<(), SiteError> {
    if depth > MAX_DEPTH {
        return Err(SiteError::TooLarge);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SiteError::Unusual(format!("{prefix}<non-UTF-8>")))?;
        let path = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(SiteError::Unusual(path));
        }
        if kind.is_dir() {
            read_dir(&entry.path(), &format!("{path}/"), depth + 1, files, total)?;
            continue;
        }
        let size = entry.metadata()?.len();
        *total = total.saturating_add(size);
        if files.len() >= MAX_FILES || *total > MAX_TOTAL_BYTES {
            return Err(SiteError::TooLarge);
        }
        let body = Bytes::from(std::fs::read(entry.path())?);
        files.insert(
            path.clone(),
            Asset {
                body,
                content_type: content_type(&name),
                immutable: path.starts_with("_app/immutable/"),
            },
        );
    }
    Ok(())
}

/// `Content-Type` by extension; anything unknown is served as opaque bytes (and `nosniff`).
pub fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The `'sha256-…'` source of every inline (no `src`) non-empty `<script>` in `html`.
pub fn inline_script_hashes(html: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let tag = &after[..open_end];
        let body_start = open_end + 1;
        let Some(close) = after[body_start..].find("</script>") else {
            break;
        };
        let body = &after[body_start..body_start + close];
        if !tag.contains("src=") && !body.is_empty() {
            hashes.push(format!(
                "'sha256-{}'",
                super::base64(&Sha256::digest(body.as_bytes()))
            ));
        }
        rest = &after[body_start + close..];
    }
    hashes
}

/// The `'sha256-…'` source of every static `style="…"` attribute in a script's templates
/// (one without interpolation or escapes: what a template inserts verbatim).
pub fn style_attribute_hashes(script: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut rest = script;
    while let Some(start) = rest.find(" style=\"") {
        let after = &rest[start + " style=\"".len()..];
        let Some(end) = after.find('"') else { break };
        let value = &after[..end];
        if !value.is_empty()
            && value.len() <= 1_024
            && !value.contains("${")
            && !value.contains('\\')
        {
            hashes.push(format!(
                "'sha256-{}'",
                super::base64(&Sha256::digest(value.as_bytes()))
            ));
        }
        rest = &after[end..];
    }
    hashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_scripts_are_hashed_and_external_ones_are_not() {
        let html = r#"<html><head><script>start()</script><script type="module" src="/_app/x.js"></script></head><body><script>
  one(); two();
</script></body></html>"#;
        let hashes = inline_script_hashes(html);
        // sha256("start()") in base64, computed outside this code (as AI Operations Layer's
        // test pins it).
        assert_eq!(
            hashes[0],
            "'sha256-DIm7WJS6ZKDYe5qFLPy+h4JFI9Bol5QmYC57mt3Fb00='"
        );
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn static_style_attributes_in_templates_are_hashed() {
        // SvelteKit's route announcer, as its build emits it; the hash is the one Chromium
        // asks for when the policy refuses it.
        let js = r#"var r=$.from_html(`<div id="svelte-announcer" aria-live="assertive" aria-atomic="true" style="position: absolute; left: 0; top: 0; clip: rect(0 0 0 0); clip-path: inset(50%); overflow: hidden; white-space: nowrap; width: 1px; height: 1px"><!></div>`);var d=`<p style="color: ${c}"></p>`;"#;
        assert_eq!(
            style_attribute_hashes(js),
            vec!["'sha256-S8qMpvofolR8Mpjy4kQvEm7m1q8clzU4dfDH0AmvZjo='".to_owned()]
        );
    }

    #[test]
    fn a_site_is_bounded_and_refuses_links() {
        let dir = std::env::temp_dir().join(format!("ps-site-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(dir.join("_app/immutable")).unwrap();
        std::fs::write(dir.join("index.html"), "<script>x()</script>").unwrap();
        std::fs::write(dir.join("_app/immutable/a.js"), "export{}").unwrap();
        let site = Site::load(&dir).unwrap();
        assert!(site.get("_app/immutable/a.js").unwrap().immutable);
        assert_eq!(
            site.get("index.html").unwrap().content_type,
            "text/html; charset=utf-8"
        );
        assert!(site.script_hashes().starts_with("'sha256-"));
        std::os::unix::fs::symlink("/etc/hosts", dir.join("hosts")).unwrap();
        assert!(matches!(Site::load(&dir), Err(SiteError::Unusual(_))));
        std::fs::remove_file(dir.join("hosts")).unwrap();
        std::fs::remove_file(dir.join("index.html")).unwrap();
        assert!(matches!(Site::load(&dir), Err(SiteError::NoIndex)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
